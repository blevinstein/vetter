//! Lifecycle smoke test for `vetterd`.
//!
//! Spawns the binary against a tempdir socket, waits for the socket
//! file to appear, sends SIGTERM, and asserts the daemon exits
//! cleanly and removes the socket file on its way out.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn wait_for_socket(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn vetterd_starts_listens_and_cleans_up_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("smoke.sock");
    let pidfile = dir.path().join("smoke.pid");
    let audit = dir.path().join("audit.log");
    let allow = dir.path().join("allow.yaml");
    std::fs::write(&allow, "rules: []\ndeny: []\n").unwrap();

    let bin = assert_cmd::cargo::cargo_bin("vetterd");
    let mut child = Command::new(bin)
        .env("VETTERD_SOCKET", &socket)
        .env("VETTERD_PIDFILE", &pidfile)
        .env("VETTER_AUDIT_LOG", &audit)
        .env("VETTER_ALLOWLIST", &allow)
        // Smoke test only cares about lifecycle (bind / pidfile /
        // SIGTERM / cleanup). The macOS-default `mac` notifier
        // would refuse to start because the cargo-built binary
        // doesn't live in a `.app` bundle; pin `noop` so the test
        // exercises the same accept-loop path on every platform.
        .env("VETTERD_NOTIFIER", "noop")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn vetterd");

    assert!(
        wait_for_socket(&socket, Duration::from_secs(5)),
        "socket never appeared at {}",
        socket.display()
    );
    assert!(
        pidfile.exists(),
        "pidfile must exist while daemon is running ({})",
        pidfile.display()
    );
    let pid_body = std::fs::read_to_string(&pidfile).expect("read pidfile");
    let recorded_pid: u32 = pid_body
        .lines()
        .next()
        .expect("pidfile non-empty")
        .trim()
        .parse()
        .expect("pidfile pid line parses as u32");
    assert_eq!(
        recorded_pid,
        child.id(),
        "pidfile records the wrong pid: {pid_body:?}"
    );

    unsafe {
        let _ = kill(child.id() as i32, 15); // SIGTERM
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    let exit = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(exit.is_some(), "daemon did not exit within 2s of SIGTERM");
    let status = exit.unwrap();
    assert!(
        status.success(),
        "daemon exited non-zero on clean shutdown: {status:?}"
    );
    assert!(
        !socket.exists(),
        "socket file should be removed on clean shutdown"
    );
    assert!(
        !pidfile.exists(),
        "pidfile should be removed on clean shutdown"
    );
}
