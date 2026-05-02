//! Tests for [`crate::socket`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use crate::testutil::tmpdir;

#[test]
fn listen_creates_socket_with_0600_perms() {
    let dir = tmpdir("vetterd-socket-test-");
    let path = dir.path().join("a.sock");
    let _l = listen(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
}

#[test]
fn listen_replaces_orphaned_socket_file() {
    let dir = tmpdir("vetterd-socket-test-");
    let path = dir.path().join("orphan.sock");
    std::fs::write(&path, b"").unwrap();
    let _l = listen(&path).expect("orphan should be unlinked and rebound");
}

#[test]
fn listen_refuses_when_live_peer_present() {
    let dir = tmpdir("vetterd-socket-test-");
    let path = dir.path().join("live.sock");
    let _alive = listen(&path).unwrap();
    let err = listen(&path).expect_err("second bind must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
}

#[test]
fn listen_creates_parent_dirs() {
    let dir = tmpdir("vetterd-socket-test-");
    let path = dir.path().join("nested/dir/a.sock");
    let _l = listen(&path).unwrap();
    assert!(path.exists());
}
