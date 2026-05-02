//! YAML allowlist loader + project discovery.
//!
//! - User scope: `$XDG_CONFIG_HOME/vet/allowlist.yaml`, falling back to
//!   `$HOME/.config/vet/allowlist.yaml`.
//! - Project scope: walk up from `cwd` looking for `.vet/allowlist.yaml`,
//!   stopping at any directory containing `.git`.
//! - Override: when `Some(path)` is passed to [`load_default`], that
//!   file becomes the **sole** source — discovery is bypassed entirely.
//!
//! The file format:
//!
//! ```yaml
//! rules:
//!   - id: foo
//!     when: { http: { method: [GET] } }
//! deny:
//!   - id: no-prod-writes
//!     when: { http: { method: [POST] } }
//! ```
//!
//! `rules` are **allow** rules; `deny` are denylist rules. Duplicate
//! `id`s within a single file are rejected at load time so the audit
//! trail stays unambiguous.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::matcher::rule::Rule;

/// Aggregated rules indexed by [`crate::matcher::Scope`].
///
/// All fields are public so tests (and the future daemon) can build
/// stores in-memory without going through YAML.
#[derive(Debug, Default, Clone)]
pub struct AllowlistStore {
    pub denylist: Vec<Rule>,
    pub session: Vec<Rule>,
    pub project: Vec<Rule>,
    pub user: Vec<Rule>,
    pub builtin: Vec<Rule>,
}

/// Wire-level YAML schema for one allowlist file.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowlistFile {
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub deny: Vec<Rule>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("serialising allowlist for {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("duplicate rule id `{id}` in {path}")]
    DuplicateId { id: String, path: PathBuf },
    #[error("no rule with id `{id}` in {path}")]
    RuleNotFound { id: String, path: PathBuf },
}

/// Load the layered store. With `override_path` set, that single file
/// becomes the sole source (rules → user scope, deny → denylist) and
/// discovery is skipped. Otherwise discovery walks up from `cwd` for
/// the project file and reads the user file from XDG.
///
/// Missing files are silently treated as empty — the daemon decides
/// what to escalate when the result is `Decision::Prompt`.
pub fn load_default(
    cwd: Option<&Path>,
    override_path: Option<&Path>,
) -> Result<AllowlistStore, LoadError> {
    if let Some(path) = override_path {
        let file = load_file(path)?;
        return Ok(AllowlistStore {
            denylist: file.deny,
            session: vec![],
            project: file.rules,
            user: vec![],
            builtin: vec![],
        });
    }

    let mut store = AllowlistStore::default();

    if let Some(user_path) = user_allowlist_path() {
        if user_path.exists() {
            let file = load_file(&user_path)?;
            store.denylist.extend(file.deny);
            store.user = file.rules;
        }
    }

    if let Some(start) = cwd {
        if let Some(project_root) = discover_project_root(start) {
            let project_path = project_root.join(".vet").join("allowlist.yaml");
            if project_path.exists() {
                let file = load_file(&project_path)?;
                store.denylist.extend(file.deny);
                store.project = file.rules;
            }
        }
    }

    Ok(store)
}

/// Walk up from `start` until either:
/// - a directory containing `.vet/allowlist.yaml` is found → return it.
/// - a directory containing `.git` is found (with or without `.vet/`)
///   → stop and return that directory if it has `.vet/allowlist.yaml`,
///   else `None`. The `.git` boundary keeps `vet` from accidentally
///   picking up rules from a parent monorepo.
/// - the filesystem root is reached without either → `None`.
pub fn discover_project_root(start: &Path) -> Option<PathBuf> {
    let mut here: Option<&Path> = Some(start);
    while let Some(dir) = here {
        let allowlist = dir.join(".vet").join("allowlist.yaml");
        let git = dir.join(".git");
        if allowlist.exists() {
            return Some(dir.to_path_buf());
        }
        if git.exists() {
            return None;
        }
        here = dir.parent();
    }
    None
}

/// Compute the path to the user-scope allowlist file from environment.
/// Returns `None` when neither `XDG_CONFIG_HOME` nor `HOME` is set —
/// the CLI surfaces that as an actionable error rather than guessing.
pub fn user_allowlist_path() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() {
            return Some(xdg.join("vet").join("allowlist.yaml"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("vet")
            .join("allowlist.yaml"),
    )
}

/// Read + parse one YAML file. Public so integration tests can drive
/// it directly and so the daemon can validate user input before
/// persisting it.
pub fn load_file(path: &Path) -> Result<AllowlistFile, LoadError> {
    let raw = std::fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file: AllowlistFile = serde_yaml_ng::from_str(&raw).map_err(|source| LoadError::Yaml {
        path: path.to_path_buf(),
        source,
    })?;
    enforce_unique_ids(&file, path)?;
    Ok(file)
}

fn enforce_unique_ids(file: &AllowlistFile, path: &Path) -> Result<(), LoadError> {
    let mut seen = HashSet::new();
    for r in file.rules.iter().chain(file.deny.iter()) {
        if !seen.insert(r.id.as_str()) {
            return Err(LoadError::DuplicateId {
                id: r.id.clone(),
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Atomically write `file` to `path`, creating parent dirs as needed.
///
/// Atomicity is via `tempfile::NamedTempFile::persist`: the YAML is
/// written to a sibling tempfile in the same directory and then
/// renamed over `path`. If the process is killed before the rename,
/// the prior file (if any) is intact.
pub fn write_file(path: &Path, file: &AllowlistFile) -> Result<(), LoadError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| LoadError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let yaml = serde_yaml_ng::to_string(file).map_err(|source| LoadError::Serialize {
        path: path.to_path_buf(),
        source,
    })?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = match parent {
        Some(p) => tempfile::NamedTempFile::new_in(p),
        None => tempfile::NamedTempFile::new_in("."),
    }
    .map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    {
        use std::io::Write as _;
        let mut handle = tmp.as_file();
        handle
            .write_all(yaml.as_bytes())
            .map_err(|source| LoadError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        handle.sync_all().map_err(|source| LoadError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    tmp.persist(path).map_err(|e| LoadError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

/// Append `rule` to `path`'s `rules:` list, creating the file if it
/// does not exist. Rejects rules whose `id` is already present (in
/// either `rules:` or `deny:`).
pub fn add_rule(path: &Path, rule: Rule) -> Result<(), LoadError> {
    let mut file = if path.exists() {
        load_file(path)?
    } else {
        AllowlistFile::default()
    };
    file.rules.push(rule);
    enforce_unique_ids(&file, path)?;
    write_file(path, &file)
}

/// Remove the first rule with the given `id` from `path`. Searches
/// `rules:` first, then `deny:`. Returns `LoadError::RuleNotFound`
/// if the id is absent.
pub fn remove_rule(path: &Path, id: &str) -> Result<(), LoadError> {
    let mut file = load_file(path)?;
    if let Some(idx) = file.rules.iter().position(|r| r.id == id) {
        file.rules.remove(idx);
    } else if let Some(idx) = file.deny.iter().position(|r| r.id == id) {
        file.deny.remove(idx);
    } else {
        return Err(LoadError::RuleNotFound {
            id: id.to_string(),
            path: path.to_path_buf(),
        });
    }
    write_file(path, &file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::rule::{HttpClause, RuleWhen};
    use crate::HttpMethod;
    use tempfile::TempDir;

    fn rule(id: &str, methods: Vec<HttpMethod>) -> Rule {
        Rule {
            id: id.to_string(),
            command: None,
            when: RuleWhen {
                http: Some(HttpClause {
                    method: Some(methods),
                    url: None,
                    headers_allow: Some(vec!["*".into()]),
                    no_body: None,
                    query: None,
                }),
                file_write: None,
                file_read: None,
            },
            note: None,
            created_by: None,
            created_at: None,
        }
    }

    #[test]
    fn allowlist_file_round_trip_minimal() {
        let yaml = r#"
rules:
  - id: ok
    when:
      http:
        method: [GET]
"#;
        let f: AllowlistFile = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(f.rules.len(), 1);
        assert!(f.deny.is_empty());
    }

    #[test]
    fn allowlist_file_rejects_unknown_top_level_key() {
        let yaml = r#"
rules: []
made_up: 1
"#;
        let err = serde_yaml_ng::from_str::<AllowlistFile>(yaml).unwrap_err();
        assert!(err.to_string().contains("made_up"), "{err}");
    }

    #[test]
    fn write_file_then_load_file_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/sub/allowlist.yaml");
        let file = AllowlistFile {
            rules: vec![rule("a", vec![HttpMethod::Get])],
            deny: vec![rule("b", vec![HttpMethod::Post])],
        };
        write_file(&path, &file).unwrap();
        let back = load_file(&path).unwrap();
        assert_eq!(back.rules.len(), 1);
        assert_eq!(back.rules[0].id, "a");
        assert_eq!(back.deny.len(), 1);
        assert_eq!(back.deny[0].id, "b");
    }

    #[test]
    fn add_rule_creates_missing_file_with_parents() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a/b/c/allowlist.yaml");
        add_rule(&path, rule("only", vec![HttpMethod::Get])).unwrap();
        assert!(path.exists());
        let back = load_file(&path).unwrap();
        assert_eq!(back.rules.len(), 1);
        assert_eq!(back.rules[0].id, "only");
    }

    #[test]
    fn add_rule_preserves_existing_rules_and_deny() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlist.yaml");
        let initial = AllowlistFile {
            rules: vec![rule("first", vec![HttpMethod::Get])],
            deny: vec![rule("blocked", vec![HttpMethod::Post])],
        };
        write_file(&path, &initial).unwrap();
        add_rule(&path, rule("second", vec![HttpMethod::Head])).unwrap();
        let back = load_file(&path).unwrap();
        assert_eq!(back.rules.len(), 2);
        assert_eq!(back.rules[0].id, "first");
        assert_eq!(back.rules[1].id, "second");
        assert_eq!(back.deny.len(), 1);
        assert_eq!(back.deny[0].id, "blocked");
    }

    #[test]
    fn add_rule_rejects_duplicate_id() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlist.yaml");
        add_rule(&path, rule("dup", vec![HttpMethod::Get])).unwrap();
        let err = add_rule(&path, rule("dup", vec![HttpMethod::Head])).unwrap_err();
        assert!(matches!(err, LoadError::DuplicateId { ref id, .. } if id == "dup"));
    }

    #[test]
    fn remove_rule_from_rules_list() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlist.yaml");
        let initial = AllowlistFile {
            rules: vec![
                rule("keep1", vec![HttpMethod::Get]),
                rule("drop", vec![HttpMethod::Head]),
                rule("keep2", vec![HttpMethod::Get]),
            ],
            deny: vec![rule("blocked", vec![HttpMethod::Post])],
        };
        write_file(&path, &initial).unwrap();
        remove_rule(&path, "drop").unwrap();
        let back = load_file(&path).unwrap();
        let ids: Vec<_> = back.rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["keep1", "keep2"]);
        assert_eq!(back.deny.len(), 1);
        assert_eq!(back.deny[0].id, "blocked");
    }

    #[test]
    fn remove_rule_from_deny_list() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlist.yaml");
        let initial = AllowlistFile {
            rules: vec![rule("keep", vec![HttpMethod::Get])],
            deny: vec![rule("drop", vec![HttpMethod::Post])],
        };
        write_file(&path, &initial).unwrap();
        remove_rule(&path, "drop").unwrap();
        let back = load_file(&path).unwrap();
        assert!(back.deny.is_empty());
        assert_eq!(back.rules.len(), 1);
    }

    #[test]
    fn remove_rule_unknown_id_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlist.yaml");
        write_file(
            &path,
            &AllowlistFile {
                rules: vec![rule("only", vec![HttpMethod::Get])],
                deny: vec![],
            },
        )
        .unwrap();
        let err = remove_rule(&path, "missing").unwrap_err();
        assert!(matches!(err, LoadError::RuleNotFound { ref id, .. } if id == "missing"));
    }
}
