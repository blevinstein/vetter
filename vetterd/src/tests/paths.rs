//! Tests for [`crate::paths`]. Layout convention is described in
//! `AGENTS.md`.

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

fn unset_runtime_source() -> Vec<Guard> {
    if cfg!(target_os = "macos") {
        vec![Guard::unset("HOME")]
    } else {
        vec![Guard::unset("XDG_RUNTIME_DIR")]
    }
}

#[test]
fn socket_env_override_wins() {
    let _g = lock();
    let _e = Guard::set("VETTERD_SOCKET", "/explicit/path.sock");
    assert_eq!(default_socket_path(), PathBuf::from("/explicit/path.sock"));
}

#[test]
fn socket_falls_back_to_per_uid_tmpdir_subdir() {
    let _g = lock();
    let _e = Guard::unset("VETTERD_SOCKET");
    let _runtime = unset_runtime_source();
    let _t = Guard::set("TMPDIR", "/some/tmp");
    let path = default_socket_path();
    let s = path.to_string_lossy();
    assert!(
        s.starts_with("/some/tmp/vetter-") && s.ends_with("/vetter.sock"),
        "expected per-uid subdir under $TMPDIR, got {s}"
    );
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
