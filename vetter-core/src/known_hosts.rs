//! Known-hosts list: a layered catalogue of host patterns that are considered
//! "familiar" to the user. Being listed does **not** grant any permission —
//! the list is purely a signal source. When an `HttpRequest` targets a host
//! that matches no entry in the store, the [`crate::signals`] module emits a
//! [`crate::signals::SignalKind::UnknownHost`] warning so the human approver
//! knows the agent is reaching somewhere unfamiliar.
//!
//! ## Storage layers (same discovery as `allowlist.yaml`)
//!
//! 1. **Built-in** — compiled into the binary; a curated short list of
//!    well-known public APIs (package registries, cloud platforms, LLM APIs).
//! 2. **User scope** — `~/.vet/known-hosts.yaml`.
//! 3. **Project scope** — `<repo>/.vet/known-hosts.yaml`; discovered by walking
//!    up from `cwd` to a `.git` boundary (same logic as the allowlist).
//!
//! ## File format
//!
//! ```yaml
//! hosts:
//!   - pattern: "api.github.com"
//!     note: "GitHub REST API"
//!   - pattern: "*.googleapis.com"
//!     note: "Google Cloud APIs"
//! ```
//!
//! Host patterns follow the same glob rules as allowlist `url.host` fields:
//! exact match (case-insensitive) or a leading `*.` wildcard that matches any
//! subdomain but **not** the apex. See [`crate::matcher::glob::matches_host`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::matcher::glob::matches_host;
use crate::matcher::loader::discover_project_root;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single entry in a known-hosts file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownHostEntry {
    /// Host pattern: exact (`api.github.com`) or wildcard (`*.github.com`).
    pub pattern: String,
    /// Optional human-readable label shown in `vet allow list` output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Wire-level YAML schema for one known-hosts file.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownHostsFile {
    #[serde(default)]
    pub hosts: Vec<KnownHostEntry>,
}

/// Aggregated known-host entries across all layers.
///
/// Entries are checked from most-specific (project) to least-specific
/// (builtin). For the `contains` query the ordering doesn't matter
/// (all layers are consulted), but keeping them separate lets the
/// CLI surface where a match came from in the future.
#[derive(Debug, Default, Clone)]
pub struct KnownHostsStore {
    pub builtin: Vec<KnownHostEntry>,
    pub user: Vec<KnownHostEntry>,
    pub project: Vec<KnownHostEntry>,
}

// ---------------------------------------------------------------------------
// Built-in host list
// ---------------------------------------------------------------------------

/// Curated list of well-known public hosts baked into the binary.
/// Format: `(pattern, note)`. Keep sorted for readability.
const BUILTIN_HOSTS: &[(&str, &str)] = &[
    // LLM API providers
    ("api.anthropic.com", "Anthropic API"),
    ("api.openai.com", "OpenAI API"),
    // AWS
    ("*.amazonaws.com", "AWS services"),
    // Azure
    ("*.azure.com", "Azure services"),
    ("*.core.windows.net", "Azure Blob / Queue / Table storage"),
    // CDN / delivery
    ("cdn.jsdelivr.net", "jsDelivr CDN"),
    // crates.io
    ("crates.io", "crates.io registry"),
    ("static.crates.io", "crates.io static assets"),
    // GitHub
    ("api.github.com", "GitHub REST API"),
    ("github.com", "GitHub"),
    ("objects.githubusercontent.com", "GitHub object storage"),
    ("raw.githubusercontent.com", "GitHub raw content"),
    // GitLab
    ("gitlab.com", "GitLab"),
    // Google Cloud
    ("*.googleapis.com", "Google Cloud APIs"),
    ("*.googleusercontent.com", "Google user content"),
    ("storage.googleapis.com", "Google Cloud Storage"),
    // npm / Yarn
    ("registry.npmjs.org", "npm registry"),
    ("registry.yarnpkg.com", "Yarn registry"),
    // PyPI
    ("files.pythonhosted.org", "PyPI file downloads"),
    ("pypi.org", "PyPI"),
    // RubyGems
    ("rubygems.org", "RubyGems"),
];

// ---------------------------------------------------------------------------
// Store construction
// ---------------------------------------------------------------------------

impl KnownHostsStore {
    /// Build a store populated only with the built-in entries.
    pub fn builtin_only() -> Self {
        Self {
            builtin: builtin_entries(),
            user: vec![],
            project: vec![],
        }
    }

    /// Return `true` if `host` matches any entry in any layer.
    pub fn contains(&self, host: &str) -> bool {
        self.project
            .iter()
            .chain(self.user.iter())
            .chain(self.builtin.iter())
            .any(|e| matches_host(&e.pattern, host))
    }
}

fn builtin_entries() -> Vec<KnownHostEntry> {
    BUILTIN_HOSTS
        .iter()
        .map(|(pattern, note)| KnownHostEntry {
            pattern: pattern.to_string(),
            note: Some(note.to_string()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum KnownHostsError {
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
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

/// Load the layered known-hosts store. Discovery mirrors
/// [`crate::matcher::loader::load_default`]:
///
/// - User scope: `~/.vet/known-hosts.yaml` (from `$HOME`).
/// - Project scope: `<repo>/.vet/known-hosts.yaml`; discovered by walking up
///   from `cwd` until a `.git` boundary.
///
/// Missing files are silently skipped; the built-in list is always present.
pub fn load_default(cwd: Option<&Path>) -> Result<KnownHostsStore, KnownHostsError> {
    let mut store = KnownHostsStore {
        builtin: builtin_entries(),
        user: vec![],
        project: vec![],
    };

    if let Some(user_path) = user_known_hosts_path() {
        if user_path.exists() {
            let file = load_file(&user_path)?;
            store.user = file.hosts;
        }
    }

    if let Some(start) = cwd {
        if let Some(project_root) = discover_project_root(start) {
            let project_path = project_root.join(".vet").join("known-hosts.yaml");
            if project_path.exists() {
                let file = load_file(&project_path)?;
                store.project = file.hosts;
            }
        }
    }

    Ok(store)
}

/// Compute the path to the user-scope known-hosts file.
/// Returns `None` when `$HOME` is unset.
pub fn user_known_hosts_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".vet").join("known-hosts.yaml"))
}

/// Read and parse one YAML file.
pub fn load_file(path: &Path) -> Result<KnownHostsFile, KnownHostsError> {
    let raw = std::fs::read_to_string(path).map_err(|source| KnownHostsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml_ng::from_str(&raw).map_err(|source| KnownHostsError::Yaml {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[path = "tests/known_hosts.rs"]
mod tests;
