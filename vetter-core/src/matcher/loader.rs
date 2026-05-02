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
    #[error("duplicate rule id `{id}` in {path}")]
    DuplicateId { id: String, path: PathBuf },
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

fn user_allowlist_path() -> Option<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
