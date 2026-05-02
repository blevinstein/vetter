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
    assert_eq!(
        default_socket_path(),
        PathBuf::from("/some/tmp/vetter.sock")
    );
    let _t2 = Guard::unset("TMPDIR");
    assert_eq!(default_socket_path(), PathBuf::from("/tmp/vetter.sock"));
}

#[test]
fn pidfile_env_override_wins() {
    let _g = lock();
    let _e = Guard::set("VETTERD_PIDFILE", "/explicit/vetter.pid");
    let sock = PathBuf::from("/anywhere/else/vetter.sock");
    assert_eq!(
        default_pidfile_path(&sock),
        PathBuf::from("/explicit/vetter.pid")
    );
}

#[test]
fn pidfile_defaults_to_socket_sibling() {
    let _g = lock();
    let _e = Guard::unset("VETTERD_PIDFILE");
    let sock = PathBuf::from("/some/dir/vetter.sock");
    assert_eq!(
        default_pidfile_path(&sock),
        PathBuf::from("/some/dir/vetter.pid")
    );
}

#[test]
fn pidfile_falls_back_to_tmpdir_when_socket_has_no_parent() {
    let _g = lock();
    let _e = Guard::unset("VETTERD_PIDFILE");
    let _t = Guard::set("TMPDIR", "/scratch");
    let sock = PathBuf::from("vetter.sock");
    assert_eq!(
        default_pidfile_path(&sock),
        PathBuf::from("/scratch/vetter.pid")
    );
}
