//! Tests for [`crate::socket_dir`]. Layout convention from `AGENTS.md`.

use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn verify_passes_on_0700_self_owned_dir() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("run");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();

    let sock = parent.join("vetter.sock");
    verify_socket_parent(&sock).expect("0700 self-owned dir should pass");
}

#[test]
fn verify_passes_on_0700_exact() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

    let sock = dir.path().join("vetter.sock");
    verify_socket_parent(&sock).expect("0700 tempdir should pass");
}

#[test]
fn verify_rejects_group_readable_dir() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("run");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).unwrap();

    let sock = parent.join("vetter.sock");
    let err = verify_socket_parent(&sock).expect_err("0750 should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("0750"), "error should mention the mode: {msg}");
    assert!(
        msg.contains("group/other"),
        "error should explain why: {msg}"
    );
}

#[test]
fn verify_rejects_world_readable_dir() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("run");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();

    let sock = parent.join("vetter.sock");
    let err = verify_socket_parent(&sock).expect_err("0755 should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("0755"), "error should mention the mode: {msg}");
}

#[test]
fn verify_rejects_world_writable_dir() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("run");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();

    let sock = parent.join("vetter.sock");
    let err = verify_socket_parent(&sock).expect_err("0777 should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("0777"), "error should mention the mode: {msg}");
}

#[test]
fn verify_rejects_missing_parent() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("nonexistent").join("vetter.sock");
    let err = verify_socket_parent(&sock).expect_err("missing parent should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("does not exist"),
        "error should say parent doesn't exist: {msg}"
    );
}

#[test]
fn verify_rejects_non_directory_parent() {
    let dir = tempfile::tempdir().unwrap();
    let fake_parent = dir.path().join("not-a-dir");
    std::fs::write(&fake_parent, b"").unwrap();

    let sock = fake_parent.join("vetter.sock");
    let err = verify_socket_parent(&sock).expect_err("non-directory parent should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("not a directory"),
        "error should say not a directory: {msg}"
    );
}

#[test]
fn verify_rejects_wrong_owner() {
    // We can only test this if running as non-root, since root owns
    // uid 0 and can't create files owned by another uid without
    // chown (which requires root). Instead, we check a system-owned
    // directory that we know is owned by root (uid 0).
    let euid = crate::peer_cred::current_euid();
    if euid == 0 {
        eprintln!("skipping verify_rejects_wrong_owner: running as root");
        return;
    }

    // /var is owned by root (uid 0) on both macOS and Linux.
    let sock = std::path::Path::new("/var/vetter-test.sock");
    let err = verify_socket_parent(sock).expect_err("root-owned /var should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("owned by uid 0"),
        "error should mention root ownership: {msg}"
    );
}
