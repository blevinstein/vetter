//! Unix-socket listener with stale-socket cleanup and tight perms.
//!
//! The listener intentionally lives at a single fixed path per user
//! (default `$TMPDIR/vetter.sock`, override `$VETTERD_SOCKET`). On
//! startup we attempt to `connect` to any pre-existing file:
//!
//! - connect succeeds  → another `vetterd` is alive; refuse to start.
//! - connect refused / not a socket → orphan from a prior crash;
//!   `unlink` and bind ours.
//!
//! Permissions are forced to `0o600` so other local users can't
//! impersonate the approver; per-user daemon, per-user UI.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// Bind a listener at `path`, cleaning up any orphaned socket file
/// from a prior crash. Returns an error if a peer is *currently*
/// listening on the same path (we refuse to clobber a live daemon).
pub fn listen(path: &Path) -> std::io::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "another vetterd is already listening on {}",
                        path.display()
                    ),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Race: file disappeared between exists() and connect().
            }
            Err(_) => {
                std::fs::remove_file(path)?;
            }
        }
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_creates_socket_with_0600_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sock");
        let _l = listen(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    #[test]
    fn listen_replaces_orphaned_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orphan.sock");
        std::fs::write(&path, b"").unwrap();
        let _l = listen(&path).expect("orphan should be unlinked and rebound");
    }

    #[test]
    fn listen_refuses_when_live_peer_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.sock");
        let _alive = listen(&path).unwrap();
        let err = listen(&path).expect_err("second bind must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
    }

    #[test]
    fn listen_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/a.sock");
        let _l = listen(&path).unwrap();
        assert!(path.exists());
    }
}
