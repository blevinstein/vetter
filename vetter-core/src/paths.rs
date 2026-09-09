//! Default path resolution shared between `vet` and `vetterd`.
//!
//! All three runtime files (socket, pidfile, audit log) live here so
//! `vet`, `vetterd`, and `vet doctor` resolve identically. Previously
//! the audit path lived in `vetterd::paths` (daemon-only); `vet
//! doctor` now needs it too to validate the audit-dir-writable check
//! without the daemon, so the resolution moved up.

use std::path::{Path, PathBuf};

/// Errors raised resolving paths that depend on environment we
/// can't fall back from. Today only `default_audit_path` returns
/// this — the socket / pidfile resolvers always succeed because they
/// have a `$TMPDIR` last-resort fallback.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("$HOME is not set; cannot resolve audit log path")]
    HomeUnset,
}

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

/// Admin socket for management commands (`vet daemon list`, etc.).
/// Derived from the main socket path so tests that set `$VETTERD_SOCKET`
/// automatically get a co-located admin socket without extra env vars.
///
/// Override with `$VETTERD_ADMIN_SOCKET` for rare cases where caller needs
/// explicit control (e.g. cross-socket integration tests).
pub fn default_admin_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("VETTERD_ADMIN_SOCKET") {
        return PathBuf::from(p);
    }
    let mut p = default_socket_path();
    p.set_file_name("vetter-admin.sock");
    p
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

/// XDG autostart entry for the daemon:
/// `$XDG_CONFIG_HOME/autostart/vetter.desktop`, defaulting to
/// `~/.config/autostart/vetter.desktop`.
///
/// Unlike macOS's `SMAppService`, Linux autostart *is a file* — there
/// is no API to ask, only an entry to write (`plans/LinuxApp.md`
/// §5.3). Resolution lives here rather than in `vetterd` so
/// `vet doctor` and the daemon agree on which file they are talking
/// about, exactly as they already do for the socket and audit log.
///
/// Pure: never touches the filesystem. Defined on every target
/// because it is path arithmetic and its tests should run everywhere;
/// only the Linux `autostart::sys` actually writes here.
pub fn user_autostart_path() -> Result<PathBuf, PathError> {
    if let Some(p) = std::env::var_os("VETTER_AUTOSTART_ENTRY") {
        return Ok(PathBuf::from(p));
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        let p = PathBuf::from(config);
        if !p.as_os_str().is_empty() {
            return Ok(p.join("autostart").join(AUTOSTART_ENTRY_NAME));
        }
    }
    let home = std::env::var_os("HOME").ok_or(PathError::HomeUnset)?;
    Ok(PathBuf::from(home)
        .join(".config/autostart")
        .join(AUTOSTART_ENTRY_NAME))
}

/// Basename of the autostart entry. Deliberately *not*
/// `dev.vetter.daemon.desktop`: that name belongs to the installed
/// application entry (`share/applications/`, see
/// `tools/install-desktop.sh`), which exists for identity — the
/// notification `desktop-entry` hint and the window's Wayland icon.
/// This is a separate file with a different job and an absolute
/// `Exec=`, and colliding the two would make uninstalling one break
/// the other.
pub const AUTOSTART_ENTRY_NAME: &str = "vetter.desktop";

#[cfg(test)]
#[path = "tests/paths.rs"]
mod tests;
