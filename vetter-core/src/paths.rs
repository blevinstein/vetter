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

/// Resolution order for the daemon socket:
/// 1. `$VETTERD_SOCKET` if set (test/install override).
/// 2. The user-private runtime dir (`$XDG_RUNTIME_DIR/vetter/` on
///    Linux, `~/Library/Application Support/vetter/run/` on macOS).
///    Both are owned by the user and 0700, which keeps a *different*
///    UID from binding the socket path before `vetterd` starts. Same-
///    UID processes are still in scope — the peer-credential check
///    on connect is what closes that gap; this path move just makes
///    it impossible for cross-user attackers to even race for the
///    bind.
/// 3. `$TMPDIR/vetter.sock` / `/tmp/vetter.sock` as a last resort if
///    neither runtime dir is available (rare: missing `$HOME`,
///    `$XDG_RUNTIME_DIR` unset on a stripped-down Linux container).
///
/// Identical resolution on both client and daemon sides so the two
/// never disagree. Pure: never touches the filesystem. Daemon-side
/// directory creation + 0700 chmod live in `vetterd::socket::listen`.
pub fn default_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("VETTERD_SOCKET") {
        return PathBuf::from(p);
    }
    if let Some(dir) = user_runtime_dir() {
        return dir.join("vetter.sock");
    }
    // Last-resort fallback: a per-uid subdirectory of `$TMPDIR` (or
    // `/tmp`). The subdirectory is what gets chmod 0700 by the
    // daemon at bind time — we deliberately never want to chmod
    // `/tmp` itself.
    let dir = std::env::var_os("TMPDIR").unwrap_or_else(|| std::ffi::OsString::from("/tmp"));
    PathBuf::from(dir)
        .join(format!("vetter-{}", current_uid_for_path()))
        .join("vetter.sock")
}

/// Read the calling process's effective UID for use in fallback
/// path names. Linked directly to avoid a `libc` workspace
/// dependency (matches the `extern "C"` style used elsewhere).
fn current_uid_for_path() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
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
        user_runtime_dir().unwrap_or_else(|| {
            std::env::var_os("TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
        })
    });
    dir.join("vetter.pid")
}

/// Per-user runtime directory candidate, computed without touching
/// the filesystem. Returns `None` when the platform-specific source
/// (`$XDG_RUNTIME_DIR` on Linux, `$HOME` on macOS) is unset or empty.
pub fn user_runtime_dir() -> Option<PathBuf> {
    platform_runtime_dir()
}

#[cfg(target_os = "macos")]
fn platform_runtime_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    if home.as_os_str().is_empty() {
        return None;
    }
    Some(home.join("Library/Application Support/vetter/run"))
}

#[cfg(not(target_os = "macos"))]
fn platform_runtime_dir() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR")?;
    let xdg = PathBuf::from(xdg);
    if xdg.as_os_str().is_empty() {
        return None;
    }
    Some(xdg.join("vetter"))
}

#[cfg(test)]
#[path = "tests/paths.rs"]
mod tests;
