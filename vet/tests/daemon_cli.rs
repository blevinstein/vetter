//! Integration tests for `vet daemon start | stop | status`.
//!
//! Each test isolates state in its own tempdir: socket, pidfile,
//! audit log, allowlist all live under the per-test scratch path so
//! parallel runs don't collide.

mod common;

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use predicates::str::contains;
use tempfile::TempDir;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

const ALLOWLIST: &str = "rules: []\ndeny: []\n";

struct Scratch {
    dir: TempDir,
    socket: PathBuf,
    pidfile: PathBuf,
    audit: PathBuf,
    allowlist: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("vetter.sock");
        let pidfile = dir.path().join("vetter.pid");
        let audit = dir.path().join("audit.log");
        let allowlist = dir.path().join("allowlist.yaml");
        std::fs::write(&allowlist, ALLOWLIST).expect("write allowlist");
        Self {
            dir,
            socket,
            pidfile,
            audit,
            allowlist,
        }
    }

    fn vet(&self) -> assert_cmd::Command {
        let mut cmd = common::vet_cmd(&self.socket, self.dir.path());
        let vetterd_bin = assert_cmd::cargo::cargo_bin("vetterd");
        cmd.env("VETTERD_BIN", &vetterd_bin)
            .env("VETTERD_PIDFILE", &self.pidfile)
            .env("VETTER_AUDIT_LOG", &self.audit)
            .env("VETTER_ALLOWLIST", &self.allowlist)
            // `vet daemon start` inherits the parent process env when
            // it spawns `vetterd`, so this propagates through to the
            // daemon. Without it the macOS-default `mac` notifier
            // would refuse the cargo-built binary (not inside a
            // `.app`) and `daemon start` would report "vetterd
            // exited before binding socket". These tests only
            // exercise the supervision plumbing, not the UI.
            .env("VETTERD_NOTIFIER", "noop");
        cmd
    }
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

fn wait_for_socket_gone(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !path.exists()
}

fn read_pidfile(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.lines().next()?.trim().parse().ok()
}

/// Test guard that SIGTERMs whatever pid lives in the scratch's
/// pidfile when dropped, then waits for the process to exit so the
/// tempdir teardown that runs *after* this drop doesn't yank the
/// socket out from under a still-shutting-down daemon.
struct Reaper(PathBuf);
impl Drop for Reaper {
    fn drop(&mut self) {
        let Some(pid) = read_pidfile(&self.0) else {
            return;
        };
        unsafe {
            let _ = kill(pid as i32, 15);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            // `kill(pid, 0)` is the standard "is this pid alive?"
            // probe: returns 0 if the signal could be delivered, -1
            // with ESRCH if the process is gone.
            let alive = unsafe { kill(pid as i32, 0) } == 0;
            if !alive {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Last-ditch SIGKILL if SIGTERM didn't take.
        unsafe {
            let _ = kill(pid as i32, 9);
        }
    }
}

#[test]
fn start_spawns_daemon_and_socket_appears() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());

    s.vet()
        .args(["daemon", "start"])
        .assert()
        .success()
        .stdout(contains("started"))
        .stdout(contains("pid="));

    assert!(
        wait_for_socket(&s.socket, Duration::from_secs(2)),
        "socket missing after start"
    );
    assert!(s.pidfile.exists(), "pidfile missing after start");
}

#[test]
fn start_when_already_running_is_idempotent() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());

    s.vet().args(["daemon", "start"]).assert().success();
    let pid_before = read_pidfile(&s.pidfile).expect("pidfile after first start");

    s.vet()
        .args(["daemon", "start"])
        .assert()
        .success()
        .stdout(contains("already running"));

    let pid_after = read_pidfile(&s.pidfile).expect("pidfile after second start");
    assert_eq!(
        pid_before, pid_after,
        "second start should not have replaced the daemon"
    );
}

#[test]
fn status_when_not_running_exits_non_zero() {
    let s = Scratch::new();

    s.vet()
        .args(["daemon", "status"])
        .assert()
        .code(78)
        .stdout(contains("not running"));
}

#[test]
fn status_when_running_reports_pid_and_uptime() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());

    s.vet().args(["daemon", "start"]).assert().success();
    let pid = read_pidfile(&s.pidfile).expect("pidfile");

    s.vet()
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(contains("running"))
        .stdout(contains(format!("pid={pid}")))
        .stdout(contains("uptime="));
}

#[test]
fn status_with_stale_pidfile_reports_stale_and_exits_non_zero() {
    let s = Scratch::new();
    // Pidfile claims a daemon at pid 1 (init), but no socket exists.
    std::fs::write(&s.pidfile, "1\n0\n").expect("write fake pidfile");

    s.vet()
        .args(["daemon", "status"])
        .assert()
        .code(78)
        .stderr(contains("stale pidfile"));
}

#[test]
fn stop_with_no_daemon_running_is_idempotent() {
    let s = Scratch::new();

    s.vet()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(contains("no daemon running"));
}

#[test]
fn stop_terminates_running_daemon_and_removes_socket_and_pidfile() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());

    s.vet().args(["daemon", "start"]).assert().success();
    assert!(wait_for_socket(&s.socket, Duration::from_secs(2)));

    s.vet()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(contains("stopped"));

    assert!(
        wait_for_socket_gone(&s.socket, Duration::from_secs(2)),
        "socket still present after stop"
    );
    assert!(
        !s.pidfile.exists(),
        "pidfile still present after stop ({})",
        s.pidfile.display()
    );
}

#[test]
fn stop_with_stale_pidfile_cleans_up_and_exits_zero() {
    let s = Scratch::new();
    // Highly unlikely PID that's almost certainly not in use. We
    // accept either branch (cleaned-up message or successful stop)
    // because the kernel decides whether the pid is alive.
    std::fs::write(&s.pidfile, "999999\n0\n").expect("write fake pidfile");

    s.vet().args(["daemon", "stop"]).assert().success();
    assert!(
        !s.pidfile.exists(),
        "pidfile should be cleaned up after stale stop"
    );
}

// ── Phase 6a: `vet daemon approve` / `reject` ───────────────────────

impl Scratch {
    /// Like [`Scratch::vet`] but returns a plain `std::process::Command`,
    /// which can be `spawn`ed instead of run to completion.
    /// `assert_cmd::Command` only offers blocking execution, and these
    /// tests need a `vet curl` left *parked* in the daemon's pending
    /// queue while a second `vet` resolves it.
    fn vet_spawnable(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("vet"));
        let prior = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::new();
        new_path.push(self.dir.path());
        new_path.push(":");
        new_path.push(prior);
        cmd.env("PATH", new_path)
            .env("VETTERD_SOCKET", &self.socket)
            .env("VETTERD_BIN", assert_cmd::cargo::cargo_bin("vetterd"))
            .env("VETTERD_PIDFILE", &self.pidfile)
            .env("VETTER_AUDIT_LOG", &self.audit)
            .env("VETTER_ALLOWLIST", &self.allowlist)
            .env("VETTERD_NOTIFIER", "noop")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("CLICOLOR")
            .env_remove("TERM");
        cmd
    }

    /// The id of the single request parked in the daemon's pending
    /// queue, scraped from `vet daemon list`. Polls because the
    /// submit races the `list` call.
    fn wait_for_one_pending_id(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = self
                .vet_spawnable()
                .args(["daemon", "list"])
                .output()
                .expect("run vet daemon list");
            let stdout = String::from_utf8_lossy(&out.stdout);
            // `print_pending_list` formats each row as
            // "  [<ULID>] curl GET <target>".
            if let Some(id) = stdout
                .lines()
                .find_map(|l| l.trim().strip_prefix('['))
                .and_then(|rest| rest.split(']').next())
            {
                return id.to_string();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for a pending request; last list output: {stdout}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The end-to-end headless approval story: a parked `vet curl`
/// unblocks with exit 0 and actually execs the wrapped command once
/// an operator approves it from a second terminal. This is
/// `plans/LinuxApp.md` §7 step 4.
#[test]
fn approve_unblocks_parked_vet_with_exit_zero() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());
    let marker = s.dir.path().join("curl-ran");
    common::install_fake_curl(s.dir.path(), &marker);

    s.vet().args(["daemon", "start"]).assert().success();
    assert!(wait_for_socket(&s.socket, Duration::from_secs(2)));

    // Parks: the noop notifier never resolves anything, so this
    // child blocks until the admin socket says otherwise.
    let mut parked = s
        .vet_spawnable()
        .args(["curl", "https://approve-e2e.example.test/"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn parked vet");

    let id = s.wait_for_one_pending_id();

    s.vet()
        .args(["daemon", "approve", &id])
        .assert()
        .success()
        .stdout(contains("approved"))
        .stdout(contains(&id));

    let status = parked.wait().expect("wait for parked vet");
    assert_eq!(
        status.code(),
        Some(0),
        "approved request should exit 0 (curl's own status)"
    );
    assert!(marker.exists(), "approved request should have exec'd curl");
}

/// The mirror image: a rejection unblocks the parked `vet` with exit
/// 77 (`EX_NOPERM`) and the wrapped command never runs.
#[test]
fn reject_unblocks_parked_vet_with_exit_77() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());
    let marker = s.dir.path().join("curl-ran");
    common::install_fake_curl(s.dir.path(), &marker);

    s.vet().args(["daemon", "start"]).assert().success();
    assert!(wait_for_socket(&s.socket, Duration::from_secs(2)));

    let mut parked = s
        .vet_spawnable()
        .args(["curl", "https://reject-e2e.example.test/"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn parked vet");

    let id = s.wait_for_one_pending_id();

    // Resolve by prefix — the whole point of daemon-side matching.
    s.vet()
        .args(["daemon", "reject", "--reason", "test rejection", &id[..8]])
        .assert()
        .success()
        .stdout(contains("rejected"))
        .stdout(contains(&id));

    let status = parked.wait().expect("wait for parked vet");
    assert_eq!(status.code(), Some(77), "rejected request should exit 77");
    assert!(!marker.exists(), "rejected request must not exec curl");

    // The operator's note reached the audit log alongside the
    // admin-socket provenance.
    let audit = std::fs::read_to_string(&s.audit).expect("audit log");
    assert!(
        audit.contains("rejected via admin socket: test rejection"),
        "{audit}"
    );
}

/// Resolving an id the daemon has never heard of is an error, not a
/// silent success.
#[test]
fn approve_unknown_id_exits_non_zero() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());

    s.vet().args(["daemon", "start"]).assert().success();
    assert!(wait_for_socket(&s.socket, Duration::from_secs(2)));

    s.vet()
        .args(["daemon", "approve", "01ZZZZZZZZZZZZZZZZZZZZZZZZ"])
        .assert()
        .code(78)
        .stderr(contains("no pending request"));
}

/// With no daemon running there is no admin socket to talk to;
/// `approve` must say so rather than hanging or reporting success.
#[test]
fn approve_with_no_daemon_running_exits_non_zero() {
    let s = Scratch::new();

    s.vet()
        .args(["daemon", "approve", "01ZZZZZZZZZZZZZZZZZZZZZZZZ"])
        .assert()
        .code(78)
        .stderr(contains("admin socket"));
}
