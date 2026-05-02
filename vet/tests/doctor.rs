//! Integration tests for `vet doctor`.
//!
//! Each test isolates state in its own tempdir: socket, pidfile,
//! audit log, allowlist, and `$HOME` all live under the per-test
//! scratch path so parallel runs (and the developer's own real
//! `~/.vet/allowlist.yaml`) don't leak into the assertions.
//!
//! The matrix mirrors the state machine in `vet/src/doctor.rs`'s
//! `check_daemon` plus the perm + config rows: clean / running /
//! stale / orphan / loose perms / bad allowlist / unwritable audit.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use predicates::str::contains;
use tempfile::TempDir;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

const ALLOWLIST_OK: &str = "rules: []\ndeny: []\n";

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
        // Force the scratch dir to 0700 so the doctor's "socket
        // parent dir" check doesn't fire WARN on macOS, where
        // `mkdtemp(3)` (via `tempfile`) yields 0755 in some
        // configurations. The perm is what we'd recommend any user
        // run with anyway; the test just wants a known baseline.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod 0700 scratch");
        let socket = dir.path().join("vetter.sock");
        let pidfile = dir.path().join("vetter.pid");
        let audit = dir.path().join("audit.log");
        let allowlist = dir.path().join("allowlist.yaml");
        std::fs::write(&allowlist, ALLOWLIST_OK).expect("write allowlist");
        Self {
            dir,
            socket,
            pidfile,
            audit,
            allowlist,
        }
    }

    /// Build a `vet` invocation pre-loaded with scratch env. Caller
    /// adds the subcommand + flags. We point `$HOME` at the scratch
    /// dir so `user_allowlist_path()` doesn't hit the developer's
    /// real `~/.vet/allowlist.yaml`, and strip color/TTY hints so
    /// `predicates::str::contains` matches stable text.
    fn vet(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("vet").expect("vet binary");
        cmd.env("HOME", self.dir.path())
            .env("VETTERD_SOCKET", &self.socket)
            .env("VETTERD_PIDFILE", &self.pidfile)
            .env("VETTER_AUDIT_LOG", &self.audit)
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("CLICOLOR")
            .env_remove("TERM");
        cmd
    }

    /// Shorthand: `vet doctor` with no extra args.
    fn doctor(&self) -> assert_cmd::Command {
        let mut cmd = self.vet();
        cmd.arg("doctor");
        cmd
    }

    /// Variant that spawns a real `vetterd` (via `vet daemon start`)
    /// before returning. Caller is responsible for keeping a
    /// [`Reaper`] alive; the daemon process inherits the same
    /// scratch socket / pidfile / audit / allowlist as `doctor()`
    /// would otherwise see.
    fn start_daemon(&self) {
        let vetterd_bin = assert_cmd::cargo::cargo_bin("vetterd");
        let mut start = assert_cmd::Command::cargo_bin("vet").expect("vet");
        start
            .env("HOME", self.dir.path())
            .env("VETTERD_SOCKET", &self.socket)
            .env("VETTERD_PIDFILE", &self.pidfile)
            .env("VETTERD_BIN", &vetterd_bin)
            .env("VETTER_AUDIT_LOG", &self.audit)
            .env("VETTER_ALLOWLIST", &self.allowlist)
            // The macOS-default `mac` notifier refuses to install
            // on a cargo-built binary (not in a `.app` bundle). The
            // env var rides through `vet daemon start`'s
            // `Command::spawn` to the daemon.
            .env("VETTERD_NOTIFIER", "noop")
            .args(["daemon", "start"])
            .assert()
            .success();
        assert!(
            wait_for_socket(&self.socket, Duration::from_secs(5)),
            "daemon socket did not appear under {}",
            self.socket.display()
        );
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

fn read_pidfile(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.lines().next()?.trim().parse().ok()
}

/// Same shape as `daemon_cli.rs::Reaper`: SIGTERM the pid in the
/// scratch's pidfile on drop, then wait so the tempdir teardown
/// doesn't yank the socket out from under a still-shutting-down
/// daemon. Duplicated rather than shared because the test binaries
/// are compiled separately and a shared module would force every
/// test file to import it.
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
            let alive = unsafe { kill(pid as i32, 0) } == 0;
            if !alive {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            let _ = kill(pid as i32, 9);
        }
    }
}

#[test]
fn clean_state_exits_zero() {
    let s = Scratch::new();

    s.doctor()
        .assert()
        .success()
        .stdout(contains("daemon"))
        .stdout(contains("INFO"))
        .stdout(contains("not running"))
        .stdout(contains("parsers registered"))
        .stdout(contains("curl"))
        .stdout(contains("summary: 0 errors, 0 warnings"));
}

#[test]
fn running_daemon_reports_ok() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());
    s.start_daemon();
    let pid = read_pidfile(&s.pidfile).expect("pidfile after start");

    s.doctor()
        .assert()
        .success()
        .stdout(contains(format!("pid={pid}")))
        .stdout(contains("peer_uid="))
        .stdout(contains("uptime="))
        .stdout(contains("summary: 0 errors, 0 warnings"));
}

#[test]
fn stale_pidfile_dead_pid_exits_error() {
    let s = Scratch::new();
    // Write a pidfile claiming a PID that almost certainly is not
    // running, with a believable start timestamp. The kernel decides
    // the verdict, so we accept the error class regardless of whether
    // a real (foreign-owned) process happens to share the pid.
    std::fs::write(&s.pidfile, "999999\n0\n").expect("write fake pidfile");

    s.doctor()
        .assert()
        .code(78)
        .stdout(contains("ERROR"))
        .stdout(contains("stale pidfile"));
}

#[test]
fn orphan_socket_no_pidfile_exits_error() {
    let s = Scratch::new();
    // Bind the scratch socket without a matching pidfile. The
    // listener stays alive for the duration of the test (dropped at
    // the end of the function) so the doctor's metadata read sees a
    // live socket file.
    let _listener = UnixListener::bind(&s.socket).expect("bind orphan socket");

    s.doctor()
        .assert()
        .code(78)
        .stdout(contains("ERROR"))
        .stdout(contains("orphan socket"));
}

#[test]
fn loose_socket_parent_perms_warns() {
    let s = Scratch::new();
    let _r = Reaper(s.pidfile.clone());
    s.start_daemon();

    // Loosen the parent dir AFTER the daemon has bound the socket.
    // The daemon's startup chmod 0700 ran already; doctor's job is
    // to notice when the dir has been relaxed since.
    std::fs::set_permissions(s.dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("relax parent perms");

    s.doctor()
        .assert()
        .success()
        .stdout(contains("socket parent dir"))
        .stdout(contains("WARN"))
        .stdout(contains("mode 0755"));
}

#[test]
fn invalid_allowlist_override_exits_error() {
    let s = Scratch::new();
    let bad = s.dir.path().join("bad_allowlist.yaml");
    // Malformed YAML — unbalanced bracket. `serde_yaml_ng` rejects
    // before reaching the schema validator.
    std::fs::write(&bad, "rules: [\n  - id: oops\n").expect("write bad allowlist");

    // `--allowlist` is a clap `global = true` flag, so it can sit
    // before or after the `doctor` subcommand. We place it before to
    // keep the argv readable.
    s.vet()
        .args(["--allowlist".as_ref(), bad.as_os_str()])
        .arg("doctor")
        .assert()
        .code(78)
        .stdout(contains("allowlist (override)"))
        .stdout(contains("ERROR"))
        .stdout(contains("parse"));
}

#[test]
fn unwritable_audit_dir_exits_error() {
    let s = Scratch::new();
    // A 0o500 dir blocks writes for the owner; on macOS / Linux
    // OpenOptions(create=true).open on a path whose parent is read-
    // only fails with EACCES.
    let locked = s.dir.path().join("locked");
    std::fs::create_dir(&locked).expect("create locked dir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod 0500");
    let target = locked.join("audit.log");

    let result = s
        .doctor()
        .env("VETTER_AUDIT_LOG", &target)
        .assert()
        .code(78);
    result
        .stdout(contains("audit log"))
        .stdout(contains("ERROR"));

    // Restore writable perms so TempDir teardown can rm the dir.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
        .expect("restore perms");
}
