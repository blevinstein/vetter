//! Shared helpers for the wrap / fail-closed integration tests.
//!
//! Each test gets its own scratch dir; this module spawns a `vetterd`
//! against a tempdir socket, drops a sentinel "fake curl" script on a
//! tempdir PATH, and exposes both to the test body.
//!
//! Cargo compiles this module separately for each test binary, so a
//! helper unused by one binary triggers `dead_code` on that binary
//! even though another binary uses it. Allow it module-wide.
#![allow(dead_code)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Spawned daemon + the scratch dir the socket / audit log live in.
pub struct Daemon {
    child: Child,
    pub socket: PathBuf,
    pub audit: PathBuf,
    pub allowlist: PathBuf,
    _scratch: TempDir,
}

impl Daemon {
    pub fn spawn(allowlist_yaml: &str) -> Self {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket = scratch.path().join("vetter.sock");
        let audit = scratch.path().join("audit.log");
        let allowlist = scratch.path().join("allowlist.yaml");
        std::fs::write(&allowlist, allowlist_yaml).expect("write allowlist");
        let bin = assert_cmd::cargo::cargo_bin("vetterd");
        let child = Command::new(bin)
            .env("VETTERD_SOCKET", &socket)
            .env("VETTER_AUDIT_LOG", &audit)
            .env("VETTER_ALLOWLIST", &allowlist)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn vetterd");
        wait_for_socket(&socket, Duration::from_secs(5));
        Self {
            child,
            socket,
            audit,
            allowlist,
            _scratch: scratch,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        unsafe {
            let _ = kill(self.child.id() as i32, 15);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
    }
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon socket did not appear at {}", path.display());
}

/// Drop a `curl` shim on `path_dir` that touches `marker` when run.
/// The shim swallows all argv (we only care that it ran) and exits 0.
pub fn install_fake_curl(path_dir: &Path, marker: &Path) {
    let curl_path = path_dir.join("curl");
    let script = format!(
        "#!/bin/sh\nprintf '' > {marker_quoted}\nexit 0\n",
        marker_quoted = shell_quote(&marker.display().to_string()),
    );
    std::fs::write(&curl_path, script).expect("write fake curl");
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(&curl_path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&curl_path, perm).unwrap();
}

fn shell_quote(s: &str) -> String {
    // Single-quote and escape embedded single quotes the POSIX way.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Build a `vet` Command with the scratch dir prepended to PATH so
/// the fake curl is found, and `VETTERD_SOCKET` pointed at the
/// daemon. Caller is expected to add `args(...)` and `assert()`.
pub fn vet_cmd(socket: &Path, path_dir: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("vet").expect("vet binary");
    let prior = std::env::var_os("PATH").unwrap_or_default();
    let mut new_path = std::ffi::OsString::new();
    new_path.push(path_dir);
    new_path.push(":");
    new_path.push(prior);
    cmd.env("PATH", new_path)
        .env("VETTERD_SOCKET", socket)
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM");
    cmd
}
