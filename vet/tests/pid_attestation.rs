//! Client-side coverage for the PID-attestation step from
//! `plans/ThreatModel.md` T1 sequencing #1.
//!
//! Two hijacker shapes — same-UID racer that bound the daemon socket
//! before `vetterd` started — must both be rejected by `vet` before
//! it sends the request body:
//!
//! 1. No pidfile at all (the racer didn't even pretend).
//! 2. Forged pidfile body but no POSIX write-lock (the racer wrote
//!    the file but didn't `fcntl(F_SETLK)` it).
//!
//! Both should exit 78 with a stderr message that names the
//! attestation failure, and neither should let the wrapped `curl`
//! execute (the sentinel marker file must stay absent).

mod common;

use std::io::Read;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use common::{install_fake_curl, vet_cmd};

/// Spawn a non-daemon listener at `socket` to play the role of a
/// same-UID racer. Returns a join handle that drains a single
/// connection; the caller can `join` it after the `vet` process
/// exits to make sure the test doesn't leave threads behind.
fn drain_one_connection(listener: UnixListener) -> std::thread::JoinHandle<()> {
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                    let mut sink = [0u8; 64];
                    let _ = stream.read(&mut sink);
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return,
            }
        }
    })
}

#[test]
fn unlocked_pidfile_hijack_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("vetter.sock");
    let pidfile = dir.path().join("vetter.pid");
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    let listener = UnixListener::bind(&socket).expect("hijack bind");
    let drain_handle = drain_one_connection(listener);

    let assertion = vet_cmd(&socket, dir.path())
        .env("VETTERD_PIDFILE", &pidfile)
        .args(["curl", "https://example.test/"])
        .assert()
        .code(78);

    let _ = drain_handle.join();

    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("peer pid attestation failed") || stderr.contains("not locked"),
        "stderr must explain PID-attestation failure; got: {stderr}"
    );
    assert!(
        !marker.exists(),
        "fail-closed: wrapped command must not run when PID attestation fails"
    );
}

#[test]
fn forged_unlocked_pidfile_body_is_still_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("vetter.sock");
    let pidfile = dir.path().join("vetter.pid");
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    // Hijacker forges a plausible body but never `fcntl(F_SETLK)`s
    // the file. `read_locker_pid` reads `F_GETLK` and reports the
    // file as unlocked → vet bails.
    std::fs::write(&pidfile, format!("{}\n0\n", std::process::id()).as_bytes())
        .expect("write fake pidfile body");

    let listener = UnixListener::bind(&socket).expect("hijack bind");
    let drain_handle = drain_one_connection(listener);

    let assertion = vet_cmd(&socket, dir.path())
        .env("VETTERD_PIDFILE", &pidfile)
        .args(["curl", "https://example.test/"])
        .assert()
        .code(78);

    let _ = drain_handle.join();

    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("not locked") || stderr.contains("peer pid attestation failed"),
        "stderr must indicate the pidfile is unlocked; got: {stderr}"
    );
    assert!(
        !marker.exists(),
        "fail-closed on forged-but-unlocked pidfile"
    );
}
