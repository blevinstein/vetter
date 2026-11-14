//! Release-staleness logic: is the running daemon older than the latest
//! release?
//!
//! This module is pure: it parses release metadata, compares versions
//! and persists a small cache at `~/.vet/update-check.yaml`. Fetching
//! the metadata (Homebrew, GitHub) lives in the `vet` binary so
//! `vetter-core` carries no network client.
//!
//! The daemon's own version comes from [`crate::MgmtRequest::GetVersion`].
//! A daemon that cannot answer it predates version reporting and is
//! reported as [`Staleness::Behind`] with no running version.

use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::fs_secure::write_atomic;

/// How long a fetched `latest` stays authoritative before `vet`
/// refreshes it in the background.
pub const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Minimum gap between two daemon-version checks on the wrap path, so a
/// busy agent session pays for the admin round trip and sees the hint
/// once a day rather than on every command.
pub const DAEMON_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Where the latest release can be downloaded when no package manager
/// applies.
pub const RELEASES_URL: &str = "https://github.com/blevinstein/vetter/releases";

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("parsing release metadata: {0}")]
    Json(#[from] serde_json::Error),
    #[error("release metadata has no version")]
    MissingVersion,
    #[error("invalid version `{0}`: {1}")]
    BadVersion(String, semver::Error),
    #[error("$HOME is not set; cannot resolve update cache path")]
    HomeUnset,
}

/// Outcome of comparing the running daemon with the latest release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    Current,
    /// `running` is `None` when the daemon did not answer
    /// [`crate::MgmtRequest::GetVersion`].
    Behind {
        running: Option<Version>,
        latest: Version,
    },
}

pub fn staleness(running: Option<&Version>, latest: &Version) -> Staleness {
    match running {
        Some(r) if r >= latest => Staleness::Current,
        _ => Staleness::Behind {
            running: running.cloned(),
            latest: latest.clone(),
        },
    }
}

/// Parse a version string, tolerating a leading `v` (release tags).
pub fn parse_version(raw: &str) -> Result<Version, UpdateError> {
    let trimmed = raw.trim();
    Version::parse(trimmed.strip_prefix('v').unwrap_or(trimmed))
        .map_err(|e| UpdateError::BadVersion(raw.to_string(), e))
}

/// Extract the version from `brew info --cask --json=v2 <cask>` output.
pub fn parse_brew_cask_version(json: &str) -> Result<Version, UpdateError> {
    let doc: serde_json::Value = serde_json::from_str(json)?;
    let raw = doc["casks"][0]["version"]
        .as_str()
        .ok_or(UpdateError::MissingVersion)?;
    parse_version(raw)
}

/// Extract the version from a GitHub `releases/latest` response.
pub fn parse_github_release_version(json: &str) -> Result<Version, UpdateError> {
    let doc: serde_json::Value = serde_json::from_str(json)?;
    let raw = doc["tag_name"]
        .as_str()
        .ok_or(UpdateError::MissingVersion)?;
    parse_version(raw)
}

/// One-line advice for getting onto `latest`, specific to the platform's
/// install channel.
pub fn upgrade_hint() -> String {
    if cfg!(target_os = "macos") {
        "run `brew upgrade --cask vetter`, then `vet daemon restart`".to_string()
    } else {
        format!("download the latest release from {RELEASES_URL}, then run `vet daemon restart`")
    }
}

/// The warning shown to the user for a [`Staleness::Behind`] result.
pub fn notice_text(running: Option<&Version>, latest: &Version) -> String {
    let running = running.map_or_else(
        || "an unknown version that predates version reporting".to_string(),
        |v| format!("v{v}"),
    );
    format!(
        "vetterd is running {running} but v{latest} is available: {}",
        upgrade_hint()
    )
}

/// What the release lookup last learned, plus when the wrap path last
/// compared the daemon against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCache {
    /// Unix seconds of the last lookup attempt, successful or not, so a
    /// failing lookup is not retried on every command.
    pub checked_at: u64,
    /// Latest released version; `None` until a lookup has succeeded.
    #[serde(default)]
    pub latest: Option<String>,
    /// Unix seconds of the last daemon-version check on the wrap path.
    #[serde(default)]
    pub daemon_checked_at: Option<u64>,
    /// The release the daemon's own banner last announced, so each
    /// release is announced once.
    #[serde(default)]
    pub notified_version: Option<String>,
}

impl UpdateCache {
    /// The cache after a lookup attempt at `now`. A failed lookup
    /// (`latest == None`) keeps the previously known release.
    pub fn after_lookup(previous: Option<&Self>, now: u64, latest: Option<String>) -> Self {
        Self {
            checked_at: now,
            latest: latest.or_else(|| previous.and_then(|c| c.latest.clone())),
            daemon_checked_at: previous.and_then(|c| c.daemon_checked_at),
            notified_version: previous.and_then(|c| c.notified_version.clone()),
        }
    }

    pub fn is_fresh(&self, now: u64) -> bool {
        now.saturating_sub(self.checked_at) < CACHE_TTL_SECS
    }

    pub fn daemon_check_due(&self, now: u64) -> bool {
        self.daemon_checked_at
            .is_none_or(|t| now.saturating_sub(t) >= DAEMON_CHECK_INTERVAL_SECS)
    }

    pub fn latest_version(&self) -> Result<Option<Version>, UpdateError> {
        self.latest.as_deref().map(parse_version).transpose()
    }
}

pub fn cache_path() -> Result<PathBuf, UpdateError> {
    let home = std::env::var_os("HOME").ok_or(UpdateError::HomeUnset)?;
    Ok(PathBuf::from(home).join(".vet").join("update-check.yaml"))
}

/// Read the cache. A missing, unreadable or malformed file is `None`:
/// the cache is advisory and is simply refetched.
pub fn load_cache_from(path: &Path) -> Option<UpdateCache> {
    crate::yaml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn store_cache_to(path: &Path, cache: &UpdateCache) -> std::io::Result<()> {
    let yaml = crate::yaml::to_string(cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    write_atomic(path, yaml.as_bytes(), 0o600)
}

#[cfg(test)]
#[path = "tests/update.rs"]
mod tests;
