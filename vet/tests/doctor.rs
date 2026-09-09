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

    // Cargo-built binaries on macOS Apple Silicon land as
    // linker-applied ad-hoc signatures, so the new code-signing rows
    // raise WARNs in the green-path tests. We only pin the
    // error-class summary substring; warnings come and go depending
    // on host arch and signing state. See `plans/TestingPlan.md`
    // §4.7.
    s.doctor()
        .assert()
        .success()
        .stdout(contains("daemon"))
        .stdout(contains("INFO"))
        .stdout(contains("not running"))
        .stdout(contains("parsers registered"))
        .stdout(contains("curl"))
        .stdout(contains("summary: 0 errors,"));
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
        .stdout(contains("summary: 0 errors,"));
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

/// `vet doctor` always emits one `code signing (vet)` and one
/// `code signing (vetterd)` row on macOS. Tests are run from
/// cargo-built binaries which Apple Silicon's linker auto-ad-hoc-signs
/// (`Signature=adhoc`), so the rows should report WARN — not ERROR
/// (which would mean `codesign --verify` failed) and not be missing.
/// On Intel x86_64 hosts where the cargo output is truly unsigned the
/// rows will report ERROR; that's expected and acceptable per
/// `plans/TestingPlan.md` §4.7.
#[test]
#[cfg(target_os = "macos")]
fn code_signing_rows_present() {
    let s = Scratch::new();

    let assert = s.doctor().assert();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    assert!(
        stdout.contains("code signing (vet)"),
        "expected `code signing (vet)` row in:\n{stdout}"
    );
    assert!(
        stdout.contains("code signing (vetterd)"),
        "expected `code signing (vetterd)` row in:\n{stdout}"
    );
    // The bundle row only appears when both binaries are inside the
    // same `.app/Contents/MacOS/`. Cargo-built tests aren't, so we
    // assert the row is *absent* — that's the documented contract.
    assert!(
        !stdout.contains("code signing (bundle)"),
        "did not expect bundle row from cargo-built binaries; got:\n{stdout}"
    );
}

/// On Linux the code-signing row is replaced by `provenance`, which
/// reports where the binary came from rather than pretending a
/// signature exists. A cargo-built test binary is owned by no
/// package, so the row is INFO "built from source" — and never OK,
/// which would claim provenance we do not have.
#[test]
#[cfg(target_os = "linux")]
fn provenance_row_reports_built_from_source() {
    let s = Scratch::new();
    let assert = s.doctor().assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("provenance"),
        "expected a `provenance` row in:\n{stdout}"
    );
    assert!(
        stdout.contains("built from source"),
        "cargo-built binaries are owned by no package; got:\n{stdout}"
    );
}

/// The Linux-only desktop rows are always emitted, even with no
/// daemon running — they degrade to "not probed" rather than
/// vanishing, so the report shape does not change under the user.
#[test]
#[cfg(target_os = "linux")]
fn desktop_rows_present_without_a_daemon() {
    let s = Scratch::new();
    let assert = s.doctor().assert();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for row in [
        "session bus",
        "runtime dir",
        "desktop entry",
        "notifications",
        "tray",
    ] {
        assert!(stdout.contains(row), "expected `{row}` row in:\n{stdout}");
    }
}

/// On platforms with neither a signing story nor a package manager
/// story the doctor still emits the single `code signing` SKIP row.
#[test]
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn code_signing_skipped_off_macos() {
    let s = Scratch::new();
    s.doctor()
        .assert()
        .success()
        .stdout(contains("code signing"))
        .stdout(contains("SKIP"))
        .stdout(contains("macOS only"));
}

// --------------------------------------------------------------------
// Hardening §H1 / ThreatModel §T8: file-mode WARN coverage
// --------------------------------------------------------------------

/// `vet doctor` must downgrade the `audit log` row from OK to WARN
/// when the on-disk file is wider than `0600`. Exit code stays 0
/// because the verdict is WARN, not ERROR — a wide-mode pre-existing
/// audit log is annoying but not exploitable on the typical
/// single-user workstation install.
#[test]
fn loose_audit_log_perms_warn() {
    let s = Scratch::new();
    // Pre-create the audit file at a wide mode. AuditLog::open is
    // load-bearing for fresh creates only; doctor's job is to notice
    // when an existing file landed wider.
    std::fs::write(&s.audit, "").expect("create audit");
    std::fs::set_permissions(&s.audit, std::fs::Permissions::from_mode(0o644))
        .expect("loosen audit");

    s.doctor()
        .assert()
        .success()
        .stdout(contains("audit log"))
        .stdout(contains("WARN"))
        .stdout(contains("mode 0644"))
        .stdout(contains("chmod 0600"))
        .stdout(contains("summary: 0 errors,"));
}

/// Same shape for the user allowlist row.
#[test]
fn loose_user_allowlist_perms_warn() {
    let s = Scratch::new();
    let user_dir = s.dir.path().join(".vet");
    std::fs::create_dir_all(&user_dir).expect("create ~/.vet");
    let user_allowlist = user_dir.join("allowlist.yaml");
    std::fs::write(&user_allowlist, ALLOWLIST_OK).expect("write user allowlist");
    std::fs::set_permissions(&user_allowlist, std::fs::Permissions::from_mode(0o644))
        .expect("loosen user allowlist");

    s.doctor()
        .assert()
        .success()
        .stdout(contains("allowlist (user)"))
        .stdout(contains("WARN"))
        .stdout(contains("mode 0644"));
}

/// `known-hosts (user)` shares the perm-check with the allowlist row.
#[test]
fn loose_user_known_hosts_perms_warn() {
    let s = Scratch::new();
    let user_dir = s.dir.path().join(".vet");
    std::fs::create_dir_all(&user_dir).expect("create ~/.vet");
    let known = user_dir.join("known-hosts.yaml");
    std::fs::write(&known, "hosts: []\n").expect("write known-hosts");
    std::fs::set_permissions(&known, std::fs::Permissions::from_mode(0o644))
        .expect("loosen known-hosts");

    s.doctor()
        .assert()
        .success()
        .stdout(contains("known-hosts (user)"))
        .stdout(contains("WARN"))
        .stdout(contains("mode 0644"));
}

/// The shared `~/.vet/` parent dir gets its own row that flips OK to
/// WARN on a too-wide mode.
#[test]
fn loose_vetter_dir_perms_warn() {
    let s = Scratch::new();
    let user_dir = s.dir.path().join(".vet");
    std::fs::create_dir(&user_dir).expect("create ~/.vet");
    std::fs::set_permissions(&user_dir, std::fs::Permissions::from_mode(0o755))
        .expect("loosen ~/.vet");

    s.doctor()
        .assert()
        .success()
        .stdout(contains("vetter dir"))
        .stdout(contains("WARN"))
        .stdout(contains("mode 0755"));
}

/// And when nothing is loose the rows report OK / Skip without WARN.
#[test]
fn tight_perms_report_no_warnings_for_perm_rows() {
    let s = Scratch::new();
    // Build a 0700 ~/.vet/ with 0600 allowlist + known-hosts.
    let user_dir = s.dir.path().join(".vet");
    std::fs::create_dir(&user_dir).expect("create ~/.vet");
    std::fs::set_permissions(&user_dir, std::fs::Permissions::from_mode(0o700))
        .expect("0700 ~/.vet");
    let user_allowlist = user_dir.join("allowlist.yaml");
    std::fs::write(&user_allowlist, ALLOWLIST_OK).expect("write user allowlist");
    std::fs::set_permissions(&user_allowlist, std::fs::Permissions::from_mode(0o600))
        .expect("0600 allowlist");
    let known = user_dir.join("known-hosts.yaml");
    std::fs::write(&known, "hosts: []\n").expect("write known-hosts");
    std::fs::set_permissions(&known, std::fs::Permissions::from_mode(0o600))
        .expect("0600 known-hosts");

    let assert = s.doctor().assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Each of the four perm-sensitive rows present and not WARN-flagged
    // for mode reasons. (Code-signing rows may still WARN on cargo
    // builds — those are unrelated to H1.)
    for needle in [
        "audit log",
        "allowlist (user)",
        "known-hosts (user)",
        "vetter dir",
    ] {
        let line = stdout
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("missing row `{needle}` in:\n{stdout}"));
        assert!(
            !line.contains("WARN"),
            "row `{needle}` unexpectedly WARN: `{line}`"
        );
    }
}
