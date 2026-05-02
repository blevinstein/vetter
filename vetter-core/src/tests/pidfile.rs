//! Tests for [`crate::pidfile`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use std::time::{Duration, UNIX_EPOCH};
use tempfile::TempDir;

#[test]
fn write_then_read_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("vetter.pid");
    let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    write(&path, 4242, start).unwrap();
    let got = read(&path).unwrap();
    assert_eq!(got.pid, 4242);
    assert_eq!(got.start, start);
}

#[test]
fn write_creates_missing_parent_dirs() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a/b/c/vetter.pid");
    let start = UNIX_EPOCH + Duration::from_secs(123);
    write(&path, 7, start).unwrap();
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
    write(&path, 1, UNIX_EPOCH).unwrap();
    assert!(path.exists());
    remove(&path);
    assert!(!path.exists());
    // Second call should be a silent no-op even though the file is gone.
    remove(&path);
}
