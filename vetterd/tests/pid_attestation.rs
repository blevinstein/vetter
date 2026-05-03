//! Daemon-side coverage for the PID-attestation step from
//! `plans/ThreatModel.md` T1 sequencing #1.
//!
//! - Live `vetterd` must hold the POSIX write-lock on its pidfile
//!   so `pidfile::read_locker_pid` reports the daemon's pid.
//! - On clean shutdown the file is removed, and even if it lingers
//!   transiently the lock is released by the kernel as soon as the
//!   daemon's fd is closed.
//!
//! Client-side hijack negative tests (foreign process bound on the
//! socket without locking the pidfile) live under `vet/tests/` —
//! they exercise the `vet` binary directly and don't need the
//! daemon scaffolding here.

use std::time::{Duration, Instant};

use vetter_core::pidfile;

mod common;
use common::Daemon;

const ALLOWLIST: &str = r#"
rules:
  - id: example-get
    when:
      http:
        method: [GET]
        url:
          scheme: https
          host: example.test
        headers_allow: ["*"]
"#;

#[test]
fn live_daemon_holds_pidfile_lock() {
    let d = Daemon::spawn(ALLOWLIST);

    let locker = pidfile::read_locker_pid(&d.pidfile)
        .expect("pidfile must be readable while the daemon is alive");
    assert_eq!(
        locker,
        Some(d.pid()),
        "fcntl(F_GETLK) must report the daemon's pid as the lock holder"
    );

    // Sanity check: the body of the pidfile names the same pid the
    // kernel attests as the lock holder. They are written together
    // by `pidfile::acquire`, but verifying both in one place catches
    // a future refactor that pulls them apart.
    let contents = pidfile::read(&d.pidfile).expect("pidfile read while alive");
    assert_eq!(contents.pid, d.pid());
}

#[test]
fn pidfile_is_removed_when_daemon_exits() {
    let d = Daemon::spawn(ALLOWLIST);
    let pidfile_path = d.pidfile.clone();

    assert_eq!(
        pidfile::read_locker_pid(&pidfile_path).unwrap(),
        Some(d.pid())
    );

    drop(d);
    let deadline = Instant::now() + Duration::from_secs(5);
    while pidfile_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !pidfile_path.exists(),
        "vetterd must remove the pidfile on clean shutdown"
    );
}
