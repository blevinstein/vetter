//! Default path resolution for the daemon.
//!
//! These are the paths the binary uses when no `VETTERD_SOCKET` /
//! `VETTER_AUDIT_LOG` env override is set. Kept in their own module so
//! tests can drive the resolution without spawning the binary.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("$HOME is not set; cannot resolve audit log path")]
    HomeUnset,
}

/// `$VETTERD_SOCKET` if set, else `$TMPDIR/vetter.sock`, else
/// `/tmp/vetter.sock`. Resolution mirrors what `vet` does on the
/// client side.
pub fn default_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("VETTERD_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = std::env::var_os("TMPDIR").unwrap_or_else(|| std::ffi::OsString::from("/tmp"));
    PathBuf::from(dir).join("vetter.sock")
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
