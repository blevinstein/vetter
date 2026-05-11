//! Fail-closed integration tests — `plans/TestingPlan.md` §4.9.
//!
//! No socket, no exec. The wrapped command's sentinel marker is the
//! ground truth here: if it appears, vet exec'd despite the daemon
//! being down, which is a security bug.

mod common;

use std::os::unix::fs::PermissionsExt as _;

use predicates::str::contains;

use common::{install_fake_curl, vet_cmd};

#[test]
fn missing_socket_exits_non_zero_and_does_not_exec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_dir = dir.path().join("run");
    std::fs::create_dir(&sock_dir).unwrap();
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = sock_dir.join("missing.sock"); // intentionally absent
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    let assertion = vet_cmd(&socket, dir.path())
        .args(["curl", "https://example.test/"])
        .assert()
        .failure();

    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("vetterd is not reachable")
            || stderr.contains("not running")
            || stderr.contains("No such file or directory"),
        "missing-socket message absent: {stderr:?}"
    );
    assert!(
        !stderr.contains("Do you want to allow"),
        "no TTY prompt allowed: {stderr:?}"
    );
    assert!(
        !marker.exists(),
        "fail-closed: wrapped command must not run when daemon is missing"
    );
}

#[test]
fn stale_socket_file_with_no_listener_also_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_dir = dir.path().join("run");
    std::fs::create_dir(&sock_dir).unwrap();
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = sock_dir.join("orphan.sock");
    std::fs::write(&socket, b"").unwrap();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    let assertion = vet_cmd(&socket, dir.path())
        .args(["curl", "https://example.test/"])
        .assert()
        .failure();

    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains("Do you want to allow"),
        "no TTY prompt allowed: {stderr:?}"
    );
    assert!(!marker.exists(), "fail-closed on stale socket");
    assertion.stderr(contains("vet:"));
}

#[test]
fn unparseable_command_does_not_reach_daemon_or_exec() {
    // We don't even spawn a daemon here — the parser should reject
    // before we attempt to connect. (And of course, no exec.)
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("unused.sock");
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    vet_cmd(&socket, dir.path())
        .args(["curl"]) // missing URL
        .assert()
        .code(78)
        .stderr(contains("missing required argument"));

    assert!(!marker.exists());
}
