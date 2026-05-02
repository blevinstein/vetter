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
        return Ok(PathBuf::from(home)
            .join("Library/Logs/vetter")
            .join("audit.log"));
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
mod tests {
    use super::*;

    /// Tests in this module mutate process-global env, so they take a
    /// shared mutex to run serially regardless of parallelism.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct Guard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }
    impl Guard {
        fn unset(key: &'static str) -> Self {
            let prev = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, prev }
        }
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, val);
            }
            Self { key, prev }
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn socket_env_override_wins() {
        let _g = lock();
        let _e = Guard::set("VETTERD_SOCKET", "/explicit/path.sock");
        assert_eq!(default_socket_path(), PathBuf::from("/explicit/path.sock"));
    }

    #[test]
    fn socket_falls_back_to_tmpdir_then_slash_tmp() {
        let _g = lock();
        let _e = Guard::unset("VETTERD_SOCKET");
        let _t = Guard::set("TMPDIR", "/some/tmp");
        assert_eq!(default_socket_path(), PathBuf::from("/some/tmp/vetter.sock"));
        let _t = Guard::unset("TMPDIR");
        assert_eq!(default_socket_path(), PathBuf::from("/tmp/vetter.sock"));
    }

    #[test]
    fn audit_env_override_wins() {
        let _g = lock();
        let _e = Guard::set("VETTER_AUDIT_LOG", "/explicit/audit.log");
        assert_eq!(
            default_audit_path().unwrap(),
            PathBuf::from("/explicit/audit.log")
        );
    }
}
