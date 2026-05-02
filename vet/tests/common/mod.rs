//! Shared helpers for the wrap / fail-closed integration tests.
//!
//! Each test gets its own scratch dir; this module spawns a `vetterd`
//! against a tempdir socket, drops a sentinel "fake curl" script on a
//! tempdir PATH, and exposes both to the test body.
//!
//! Cargo compiles this module separately for each test binary, so a
//! helper unused by one binary triggers `dead_code` on that binary
//! even though another binary uses it. Allow it module-wide.
//!
//! Phase 4: every spawned daemon now wires a [`MockUi`] into
//! `VETTERD_NOTIFIER=mock`. Tests that hit prompt-class paths must
//! configure the mock's response (default deny — see
//! [`MockUi::set_default`]).
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tempfile::TempDir;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Decision the [`MockUi`] responds with for a given prompt.
#[derive(Debug, Clone)]
pub enum MockKind {
    Allow,
    Deny,
}

#[derive(Debug, Clone)]
pub struct MockResponse {
    pub kind: MockKind,
    pub reason: String,
}

impl MockResponse {
    pub fn allow(reason: impl Into<String>) -> Self {
        Self {
            kind: MockKind::Allow,
            reason: reason.into(),
        }
    }
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            kind: MockKind::Deny,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptSummary {
    id: String,
    command: String,
    #[serde(default)]
    primary_verb: String,
    #[serde(default)]
    primary_target: String,
    #[serde(default)]
    force_prompt: bool,
    #[serde(default)]
    rendered: String,
}

#[derive(Default)]
struct MockState {
    by_target: HashMap<String, MockResponse>,
    default: Option<MockResponse>,
}

/// In-process driver for `VETTERD_NOTIFIER=mock`. Construct one
/// before the daemon spawns; configure responses via
/// [`set_default`] / [`expect_target`] before triggering requests.
pub struct MockUi {
    socket_path: PathBuf,
    state: Arc<Mutex<MockState>>,
}

impl MockUi {
    pub fn new(socket_path: PathBuf) -> Self {
        let state = Arc::new(Mutex::new(MockState::default()));
        let listener = UnixListener::bind(&socket_path).expect("mock ui bind");
        listener.set_nonblocking(true).expect("set_nonblocking");
        let state_for_thread = Arc::clone(&state);
        std::thread::Builder::new()
            .name("vet-mock-ui".into())
            .spawn(move || serve(listener, state_for_thread))
            .expect("spawn mock ui");
        Self { socket_path, state }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn set_default(&self, r: MockResponse) {
        self.state.lock().expect("mock state").default = Some(r);
    }

    pub fn expect_target(&self, target: &str, r: MockResponse) {
        self.state
            .lock()
            .expect("mock state")
            .by_target
            .insert(target.into(), r);
    }
}

#[derive(Serialize)]
struct WireResponse<'a> {
    decision: &'a str,
    reason: &'a str,
}

fn serve(listener: UnixListener, state: Arc<Mutex<MockState>>) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    let _ = handle_one(stream, state);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return,
        }
    }
}

fn handle_one(stream: UnixStream, state: Arc<Mutex<MockState>>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(());
    }
    let summary: PromptSummary =
        serde_json::from_str(line.trim_end()).map_err(|e| std::io::Error::other(e.to_string()))?;
    let response = {
        let g = state.lock().expect("mock state");
        g.by_target
            .get(&summary.primary_target)
            .cloned()
            .or_else(|| g.default.clone())
            .unwrap_or_else(|| MockResponse::deny("mock ui default (no rule configured)"))
    };
    let resp = WireResponse {
        decision: match response.kind {
            MockKind::Allow => "allow",
            MockKind::Deny => "deny",
        },
        reason: &response.reason,
    };
    let mut bytes = serde_json::to_vec(&resp).map_err(|e| std::io::Error::other(e.to_string()))?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

/// Spawned daemon + the scratch dir the socket / audit log live in.
pub struct Daemon {
    child: Child,
    pub socket: PathBuf,
    pub audit: PathBuf,
    pub allowlist: PathBuf,
    pub ui: MockUi,
    _scratch: TempDir,
}

impl Daemon {
    pub fn spawn(allowlist_yaml: &str) -> Self {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket = scratch.path().join("vetter.sock");
        let audit = scratch.path().join("audit.log");
        let allowlist = scratch.path().join("allowlist.yaml");
        std::fs::write(&allowlist, allowlist_yaml).expect("write allowlist");
        let ui_socket = scratch.path().join("notifier.sock");
        let ui = MockUi::new(ui_socket.clone());
        let bin = assert_cmd::cargo::cargo_bin("vetterd");
        let child = Command::new(bin)
            .env("VETTERD_SOCKET", &socket)
            .env("VETTER_AUDIT_LOG", &audit)
            .env("VETTER_ALLOWLIST", &allowlist)
            .env("VETTERD_NOTIFIER", "mock")
            .env("VETTERD_NOTIFIER_SOCKET", &ui_socket)
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
            ui,
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
