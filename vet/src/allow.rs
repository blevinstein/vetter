//! `vet allow add | rm | list` handlers.
//!
//! Read/write the YAML allowlist files from
//! [`vetter_core::matcher::loader`]. Scope selection is:
//!
//! - `--allowlist <path>` (the global flag) — write directly to that
//!   file regardless of `--scope`.
//! - `--scope user` (default) — `$HOME/.vet/allowlist.yaml`.
//! - `--scope project` — `<repo>/.vet/allowlist.yaml`, where `<repo>`
//!   is the nearest ancestor of `cwd` that contains `.git/` (or
//!   already contains `.vet/allowlist.yaml`). If no such ancestor
//!   exists we refuse rather than scribble into the wrong directory.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;

use vetter_core::matcher::{
    self, add_rule, derive_auto_id, discover_project_root, remove_rule, user_allowlist_path,
    AllowlistStore, Rule, RuleWhen, Scope,
};

use crate::messages::explain_load_error;
use crate::AllowScope;

/// Exit code for "config / cannot vet" per `plans/Overview.md` §4.
const EXIT_CONFIG: u8 = 78;

/// User-supplied YAML for `vet allow add`. Mirrors [`Rule`] except the
/// `id` field is optional — when omitted, an `auto-<hash>` id is
/// derived from the canonical serialised form.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulePattern {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    command: Option<String>,
    when: RuleWhen,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

pub fn add(pattern: &str, scope: AllowScope, override_path: Option<&Path>) -> ExitCode {
    let parsed: RulePattern = match serde_yaml_ng::from_str(pattern) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("vet allow: cannot parse pattern as YAML rule: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    if parsed.when.is_empty() {
        eprintln!("vet allow: pattern must include a non-empty `when:` clause");
        return ExitCode::from(EXIT_CONFIG);
    }

    let mut rule = Rule {
        id: parsed.id.unwrap_or_default(),
        command: parsed.command,
        when: parsed.when,
        note: parsed.note,
        created_by: parsed.created_by,
        created_at: parsed.created_at,
    };
    if rule.id.is_empty() {
        rule.id = derive_auto_id(&rule);
    }

    let target = match resolve_target(scope, override_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("vet allow: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    match add_rule(&target, rule.clone()) {
        Ok(()) => {
            println!(
                "vet allow: added rule `{}` to {}",
                rule.id,
                target.display()
            );
            println!("vet allow: note — comments in the YAML are not preserved on round-trip.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vet allow: {}", explain_load_error(&e));
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

pub fn rm(id: &str, scope: AllowScope, override_path: Option<&Path>) -> ExitCode {
    let target = match resolve_target(scope, override_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("vet allow: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    match remove_rule(&target, id) {
        Ok(()) => {
            println!("vet allow: removed rule `{}` from {}", id, target.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vet allow: {}", explain_load_error(&e));
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

pub fn list(
    scope_filter: Option<AllowScope>,
    history: bool,
    override_path: Option<&Path>,
) -> ExitCode {
    if history {
        println!(
            "vet allow: --history requires the daemon (lands in Phase 3); \
             see plans/Overview.md §7 for the audit log path."
        );
        return ExitCode::SUCCESS;
    }

    let cwd = std::env::current_dir().ok();
    let store = match matcher::load_default(cwd.as_deref(), override_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "vet allow: allowlist load failed: {}",
                explain_load_error(&e)
            );
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    let groups = ordered_scopes(&store);
    let mut printed = false;
    for (scope, rules) in groups {
        if !scope_passes_filter(scope, scope_filter) {
            continue;
        }
        for rule in rules {
            println!("[{}] {}  {}", scope.as_str(), rule.id, summarise(rule));
            printed = true;
        }
    }
    if !printed {
        println!("vet allow: no rules loaded");
    }
    ExitCode::SUCCESS
}

fn ordered_scopes(store: &AllowlistStore) -> [(Scope, &[Rule]); 4] {
    [
        (Scope::Denylist, store.denylist.as_slice()),
        (Scope::Project, store.project.as_slice()),
        (Scope::User, store.user.as_slice()),
        (Scope::Builtin, store.builtin.as_slice()),
    ]
}

fn scope_passes_filter(scope: Scope, filter: Option<AllowScope>) -> bool {
    match filter {
        None => true,
        Some(AllowScope::User) => matches!(scope, Scope::User),
        Some(AllowScope::Project) => matches!(scope, Scope::Project | Scope::Denylist),
    }
}

fn summarise(rule: &Rule) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(cmd) = &rule.command {
        parts.push(format!("cmd={cmd}"));
    }
    if let Some(http) = &rule.when.http {
        let methods = http
            .method
            .as_ref()
            .map(|m| {
                m.iter()
                    .map(|x| x.as_str().to_string())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .unwrap_or_else(|| "*".to_string());
        let url_part = match &http.url {
            Some(u) => {
                let scheme = u.scheme.as_deref().unwrap_or("*");
                let host = u
                    .host
                    .as_ref()
                    .map(|h| h.patterns().join("|"))
                    .unwrap_or_else(|| "*".to_string());
                let path = u.path.as_deref().unwrap_or("*");
                format!("{scheme}://{host}{path}")
            }
            None => "*".to_string(),
        };
        parts.push(format!("http {methods} {url_part}"));
    }
    if let Some(fw) = &rule.when.file_write {
        parts.push(format!("file_write {}", fw.path.as_deref().unwrap_or("*")));
    }
    if let Some(fr) = &rule.when.file_read {
        parts.push(format!("file_read {}", fr.path.as_deref().unwrap_or("*")));
    }
    if let Some(note) = &rule.note {
        parts.push(format!("({note})"));
    }
    if parts.is_empty() {
        "(empty when:)".to_string()
    } else {
        parts.join("  ")
    }
}

fn resolve_target(scope: AllowScope, override_path: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = override_path {
        return Ok(p.to_path_buf());
    }
    match scope {
        AllowScope::User => user_allowlist_path()
            .ok_or_else(|| "$HOME is not set; cannot resolve user allowlist path".into()),
        AllowScope::Project => {
            let cwd = std::env::current_dir()
                .map_err(|e| format!("cannot read current directory: {e}"))?;
            let root = find_project_root(&cwd).ok_or_else(|| {
                "no project root found from cwd: looked for an ancestor with `.git/` or \
                 `.vet/allowlist.yaml`. Create `.vet/` at the repository root, or use \
                 `--allowlist <path>` to target an explicit file."
                    .to_string()
            })?;
            Ok(root.join(".vet").join("allowlist.yaml"))
        }
    }
}

/// Walk up from `start` looking for either an existing
/// `.vet/allowlist.yaml` or a `.git/` directory. The first hit wins.
/// Differs from [`matcher::discover_project_root`] in that we accept
/// a `.git/` boundary as a *valid* target so `add --scope project`
/// can create the file in a fresh repo.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    if let Some(root) = discover_project_root(start) {
        return Some(root);
    }
    let mut here: Option<&Path> = Some(start);
    while let Some(dir) = here {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        here = dir.parent();
    }
    None
}
