//! User-facing daemon preferences.
//!
//! Stored at `~/.vet/settings.yaml` (sibling to `allowlist.yaml` and
//! `known-hosts.yaml`) so the user only has to remember one config
//! directory. Designed to be extended over time with additional
//! single-value preferences; today the only field is `autostart`,
//! controlling whether the macOS daemon registers itself as a Login
//! Item via [`SMAppService.mainApp`](https://developer.apple.com/documentation/servicemanagement/smappservice).
//!
//! ## File format
//!
//! ```yaml
//! autostart: true
//! ```
//!
//! Missing file → empty defaults (autostart off). Unknown fields
//! (`#[serde(deny_unknown_fields)]`) are rejected at load time so a
//! typo in the YAML surfaces as a config error rather than being
//! silently ignored.
//!
//! ## On-disk perms
//!
//! Writes land at mode `0600`. The file doesn't carry secrets today,
//! but the daemon reads it at startup as a `same-uid` trust input
//! (autostart toggling the user's login items) — we want the same
//! "no other UID can read or rewrite this" guarantee as the audit
//! log and allowlist write paths in Hardening §H1.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// User preferences persisted to `~/.vet/settings.yaml`.
///
/// `Default` matches the "no settings file present" state — every
/// field defaults to its safe / opt-out value, so a user who never
/// opens the popover or runs `vet daemon autostart enable` sees no
/// behaviour change from the daemon doing nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Register `Vetter.app` as a macOS Login Item so the daemon comes
    /// back automatically after every reboot. Off by default —
    /// `vetter` is a security tool, never auto-enable behaviours that
    /// outlive the user's explicit consent.
    #[serde(default)]
    pub autostart: bool,
}

/// User-facing summary of `[SMAppService.mainApp status]` (macOS) or
/// the equivalent state on other platforms.
///
/// Lives in `vetter_core` (rather than `vetterd::autostart`) so the
/// admin-socket wire module can reference it without pulling in the
/// daemon crate. The `vetterd::autostart` driver returns this enum
/// from its `current()` / `reconcile_with_settings()` calls; the
/// `vet` CLI prints it from `vet daemon autostart status` and `vet
/// doctor`.
///
/// Variants mirror the
/// [`SMAppServiceStatus`](https://developer.apple.com/documentation/servicemanagement/smappservicestatus)
/// enum (NotRegistered=0, Enabled=1, RequiresApproval=2,
/// NotFound=3) plus a synthetic [`Self::Unsupported`] for non-macOS
/// targets and macOS hosts older than 13.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutostartStatus {
    /// Bundle is registered and will launch at login.
    Enabled,
    /// Bundle has not been registered (default state for a fresh
    /// install). `enable()` will move us to `Enabled`.
    NotRegistered,
    /// Registration has been requested but the user must approve it
    /// in System Settings → General → Login Items before launchd will
    /// honour it. Surfacing this distinct from `Enabled` lets `vet
    /// doctor` give the user an actionable hint.
    RequiresApproval,
    /// Apple's docs describe this as "the service is not found by
    /// the system" — typically because the app is not on disk in a
    /// location launchd can `LaunchServices`-resolve.
    NotFound,
    /// The build, OS, or runtime context cannot perform login-item
    /// registration. Reachable when the daemon is invoked outside
    /// an `.app` bundle (`cargo run`, bare binary), and on every
    /// non-macOS target.
    Unsupported,
}

impl AutostartStatus {
    /// True iff the service is registered and launchd intends to run
    /// us at next login (with or without pending approval).
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled | Self::RequiresApproval)
    }

    /// Short human label used by `vet daemon autostart status` and
    /// the `vet doctor` row.
    pub fn label(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::NotRegistered => "disabled",
            Self::RequiresApproval => "requires approval",
            Self::NotFound => "not found",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("$HOME is not set; cannot resolve settings path")]
    HomeUnset,
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
    #[error("serialising settings for {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
}

/// Resolve the canonical settings path: `$HOME/.vet/settings.yaml`.
/// Mirrors [`crate::known_hosts::user_known_hosts_path`] /
/// [`crate::matcher::loader::user_allowlist_path`] so all three live
/// in the same directory.
pub fn settings_path() -> Result<PathBuf, SettingsError> {
    let home = std::env::var_os("HOME").ok_or(SettingsError::HomeUnset)?;
    Ok(PathBuf::from(home).join(".vet").join("settings.yaml"))
}

/// Load settings from the canonical user path. Missing file → returns
/// [`Settings::default`] (no error). Parse errors propagate so a typo
/// in the YAML surfaces immediately on the next load attempt rather
/// than being silently ignored.
pub fn load() -> Result<Settings, SettingsError> {
    let path = settings_path()?;
    load_from(&path)
}

/// Same as [`load`] but reads from an explicit path. Useful for
/// tests and for the `--settings-path` escape hatch a future CLI
/// flag may add.
pub fn load_from(path: &Path) -> Result<Settings, SettingsError> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = std::fs::read_to_string(path).map_err(|source| SettingsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml_ng::from_str(&raw).map_err(|source| SettingsError::Yaml {
        path: path.to_path_buf(),
        source,
    })
}

/// Persist `settings` to the canonical user path, creating
/// `~/.vet/` if it does not exist. Atomic via [`write_to`] /
/// `tempfile::NamedTempFile::persist`.
pub fn store(settings: &Settings) -> Result<(), SettingsError> {
    let path = settings_path()?;
    write_to(&path, settings)
}

/// Atomically write `settings` to `path`, creating parent dirs as
/// needed. Mode is set to `0600` on the final file so other UIDs on
/// the host can't read or rewrite the user's preferences.
///
/// Atomicity is via `tempfile::NamedTempFile::persist`: the YAML is
/// written to a sibling tempfile in the same directory and then
/// renamed over `path`. If the process is killed before the rename,
/// the prior file (if any) is intact.
pub fn write_to(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    use std::os::unix::fs::PermissionsExt as _;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let yaml = serde_yaml_ng::to_string(settings).map_err(|source| SettingsError::Serialize {
        path: path.to_path_buf(),
        source,
    })?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = match parent {
        Some(p) => tempfile::NamedTempFile::new_in(p),
        None => tempfile::NamedTempFile::new_in("."),
    }
    .map_err(|source| SettingsError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    {
        use std::io::Write as _;
        let mut handle = tmp.as_file();
        handle
            .write_all(yaml.as_bytes())
            .map_err(|source| SettingsError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        handle.sync_all().map_err(|source| SettingsError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    // Set the tempfile's mode *before* persisting so the rename
    // produces an already-0600 file rather than briefly exposing it
    // at the umask default (typically 0644). H1 ship-blocker: no
    // sensitive vetter file should land mode 0644 on disk.
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600)).map_err(
        |source| SettingsError::Io {
            path: path.to_path_buf(),
            source,
        },
    )?;
    tmp.persist(path).map_err(|e| SettingsError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/settings.rs"]
mod tests;
