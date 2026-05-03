//! Tests for [`crate::pidfile`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use std::time::{Duration, UNIX_EPOCH};
use tempfile::TempDir;

#[test]
fn acquire_writes_pid_and_start() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("vetter.pid");
    let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let lock = acquire(&path, 4242, start).unwrap();
    let got = read(&path).unwrap();
    assert_eq!(got.pid, 4242);
    assert_eq!(got.start, start);
    drop(lock);
}

#[test]
fn acquire_creates_missing_parent_dirs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a/b/c/vetter.pid");
    let start = UNIX_EPOCH + Duration::from_secs(123);
    let _lock = acquire(&path, 7, start).unwrap();
    assert!(path.exists());
    assert_eq!(read(&path).unwrap().pid, 7);
}

#[test]
fn read_missing_file_returns_not_found() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("absent.pid");
    let err = read(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn read_malformed_file_returns_invalid_data() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.pid");
    std::fs::write(&path, b"not-a-pid\n").unwrap();
    let err = read(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn read_bad_pid_returns_invalid_data() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.pid");
    std::fs::write(&path, b"abc\n123\n").unwrap();
    let err = read(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn read_bad_start_returns_invalid_data() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.pid");
    std::fs::write(&path, b"123\nabc\n").unwrap();
    let err = read(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn remove_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("vetter.pid");
    let lock = acquire(&path, 1, UNIX_EPOCH).unwrap();
    assert!(path.exists());
    drop(lock);
    remove(&path);
    assert!(!path.exists());
    // Second call should be a silent no-op even though the file is gone.
    remove(&path);
}

#[test]
fn is_pid_alive_says_true_for_self() {
    // The current process is, by definition, alive.
    assert!(is_pid_alive(std::process::id()));
}

#[test]
fn is_pid_alive_says_false_for_unlikely_pid() {
    // Picking a pid the kernel almost certainly does not have
    // assigned. PIDs are 32-bit but the typical max on Linux/macOS
    // is far below this bound, so ESRCH is the expected result.
    // The probe returns true on EPERM (alive but unsignalable) — we
    // chose a pid that should not exist at all rather than one that
    // exists but is foreign-owned.
    assert!(!is_pid_alive(u32::MAX - 1));
}

#[test]
fn read_locker_pid_for_missing_file_returns_not_found() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("absent.pid");
    let err = read_locker_pid(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn read_locker_pid_for_unlocked_file_returns_none() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("vetter.pid");
    // File exists but no process holds an fcntl write-lock on it.
    std::fs::write(&path, b"123\n456\n").unwrap();
    assert_eq!(read_locker_pid(&path).unwrap(), None);
}

// Cross-process verification of `acquire`'s lock semantics
// (a second process must observe `read_locker_pid == Some(child_pid)`
// and a `WouldBlock` from its own `acquire` attempt) lives in the
// `vetterd` integration suite, where spawning a real subprocess is
// already part of the test scaffolding. POSIX `F_GETLK` only reports
// foreign-process locks, so a same-process probe always returns
// `F_UNLCK` and would be useless here.
