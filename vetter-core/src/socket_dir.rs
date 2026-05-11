//! Pre-connect verification of the daemon socket's parent directory.
//!
//! The daemon forces the parent to `0700` at bind time
//! ([`vetterd::socket::listen`]). This module provides the client-side
//! mirror: before `vet` calls `UnixStream::connect`, it checks that
//! the parent directory (a) is owned by the calling process's EUID and
//! (b) has no group/other permission bits set. If either condition
//! fails, connecting would be unsafe — a different user (or an overly
//! permissive directory) could allow socket replacement between the
//! stat and the connect, even though peer-cred catches the mismatch
//! after the fact.
//!
//! ThreatModel T1 residual — the check is cheap belt-and-suspenders
//! on top of peer-cred + PID attestation.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use crate::peer_cred::current_euid;
use crate::wire::WireError;

/// Verify that the parent directory of `socket_path` is owned by the
/// calling process's EUID and has mode no wider than `0700`.
///
/// Returns `Ok(())` on success, or `WireError::SocketDirInsecure` /
/// `WireError::Io` on failure.
pub fn verify_socket_parent(socket_path: &Path) -> Result<(), WireError> {
    let parent = socket_path
        .parent()
        .ok_or_else(|| WireError::SocketDirInsecure {
            path: socket_path.display().to_string(),
            reason: "socket path has no parent directory".into(),
        })?;

    let meta = std::fs::metadata(parent).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            WireError::SocketDirInsecure {
                path: parent.display().to_string(),
                reason: "parent directory does not exist".into(),
            }
        } else {
            WireError::Io(e)
        }
    })?;

    if !meta.is_dir() {
        return Err(WireError::SocketDirInsecure {
            path: parent.display().to_string(),
            reason: "parent path is not a directory".into(),
        });
    }

    let owner = meta.uid();
    let expected = current_euid();
    if owner != expected {
        return Err(WireError::SocketDirInsecure {
            path: parent.display().to_string(),
            reason: format!("owned by uid {owner}, expected {expected} (our euid)"),
        });
    }

    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(WireError::SocketDirInsecure {
            path: parent.display().to_string(),
            reason: format!("mode is {mode:04o}, expected no group/other bits (at most 0700)"),
        });
    }

    Ok(())
}

#[cfg(test)]
#[path = "tests/socket_dir.rs"]
mod tests;
