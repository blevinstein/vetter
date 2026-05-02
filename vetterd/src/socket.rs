//! Unix-socket listener with stale-socket cleanup and tight perms.
//!
//! The listener intentionally lives at a single fixed path per user
//! (default `$XDG_RUNTIME_DIR/vetter/vetter.sock` on Linux,
//! `~/Library/Application Support/vetter/run/vetter.sock` on macOS,
//! override `$VETTERD_SOCKET`). On startup we attempt to `connect` to
//! any pre-existing file:
//!
//! - connect succeeds  → another `vetterd` is alive; refuse to start.
//! - connect refused / not a socket → orphan from a prior crash;
//!   `unlink` and bind ours.
//!
//! The socket file is chmod 0600 and its parent dir is chmod 0700 so
//! other local users can't impersonate the approver. Same-UID
//! impersonation is closed by the peer-credential check the client
//! runs on connect (see `vetter_core::peer_cred`).

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
                    format!("another vetterd is already listening on {}", path.display()),
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
            // Force the parent dir to 0700. Even if it already
            // existed (XDG_RUNTIME_DIR usually does, scratch
            // tempdirs always do) we want to guarantee no other UID
            // can drop a file alongside the socket and race the
            // bind. Same-UID protection is the client's
            // responsibility (peer-cred check on connect).
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(test)]
#[path = "tests/socket.rs"]
mod tests;
