//! Default path resolution for the daemon.
//!
//! The socket and pidfile defaults live in [`vetter_core::paths`] so
//! both `vet` and `vetterd` resolve identically. The audit-log path
//! is daemon-only and stays here.

use std::path::PathBuf;

pub use vetter_core::paths::{default_pidfile_path, default_socket_path};

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("$HOME is not set; cannot resolve audit log path")]
    HomeUnset,
}

/// `$VETTER_AUDIT_LOG` if set, else
/// `~/Library/Logs/vetter/audit.log` on macOS,
/// `$XDG_STATE_HOME/vetter/audit.log` (default
/// `~/.local/state/vetter/audit.log`) elsewhere.
pub fn default_audit_path() -> Result<PathBuf, PathError> {
    if let Some(p) = std::env::var_os("VETTER_AUDIT_LOG") {
        return Ok(PathBuf::from(p));
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or(PathError::HomeUnset)?;
        Ok(PathBuf::from(home)
            .join("Library/Logs/vetter")
            .join("audit.log"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
            let p = PathBuf::from(state);
            if !p.as_os_str().is_empty() {
                return Ok(p.join("vetter").join("audit.log"));
            }
        }
        let home = std::env::var_os("HOME").ok_or(PathError::HomeUnset)?;
        Ok(PathBuf::from(home)
            .join(".local/state/vetter")
            .join("audit.log"))
    }
}

#[cfg(test)]
#[path = "tests/paths.rs"]
mod tests;
