//! Default path resolution shared between `vet` and `vetterd`.
//!
//! Both binaries need to agree on where the Unix socket and the
//! supervisor pidfile live; previously each had its own copy of the
//! resolution logic. Centralising here means `vet daemon start`,
//! `vet daemon status`, and `vetterd` itself read the exact same
//! environment.
//!
//! The audit-log path remains in `vetterd::paths` because only the
//! daemon ever needs it.

use std::path::{Path, PathBuf};

/// `$VETTERD_SOCKET` if set, else `$TMPDIR/vetter.sock`, else
/// `/tmp/vetter.sock`. Identical resolution on both client and
/// daemon sides so the two never disagree.
pub fn default_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("VETTERD_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = std::env::var_os("TMPDIR").unwrap_or_else(|| std::ffi::OsString::from("/tmp"));
    PathBuf::from(dir).join("vetter.sock")
}

/// `$VETTERD_PIDFILE` if set, else a sibling of the socket called
/// `vetter.pid`. Co-locating the pidfile with the socket keeps the
/// "is the daemon up?" answer in one place per scratch tempdir, which
/// matters because integration tests parallelise over many sockets.
pub fn default_pidfile_path(socket_path: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("VETTERD_PIDFILE") {
        return PathBuf::from(p);
    }
    let parent = socket_path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = parent.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    });
    dir.join("vetter.pid")
}

#[cfg(test)]
#[path = "tests/paths.rs"]
mod tests;
