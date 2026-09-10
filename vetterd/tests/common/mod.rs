//! Shared helpers for `vetterd` integration tests.
//!
//! Lives at `tests/common/mod.rs` (the canonical Cargo location for
//! integration-test-only code that the test binaries themselves
//! aren't required to expose). Used by `daemon_e2e.rs` and
//! `daemon_e2e_prompt.rs`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use vetter_core::wire::{
    new_request_id, read_decision, write_frame, VetDecision, VetRequest, WireDecision,
    PROTOCOL_VERSION,
};

/// Decision the [`MockUi`] responds with for a given prompt. Mirrors
/// the `mock` notifier wire (see `vetterd/src/notifier/mock.rs`).
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub decision: WireDecision,
    pub reason: String,
}

impl MockResponse {
    pub fn allow(reason: impl Into<String>) -> Self {
        Self {
            decision: WireDecision::Allow,
            reason: reason.into(),
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            decision: WireDecision::Deny,
            reason: reason.into(),
        }
    }
}

/// What the mock notifier sends us per prompt, decoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSummary {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub primary_verb: String,
    #[serde(default)]
    pub primary_target: String,
    #[serde(default)]
    pub force_prompt: bool,
    /// The pre-rendered §8.5 detail block — what the macOS popover
    /// shows in its body NSTextView. Empty when the daemon called
    /// the legacy `submit` instead of `submit_with_render` (no
    /// known producer does that today; tests that want to assert
    /// the popover contract should still pin this to non-empty).
    #[serde(default)]
    pub rendered: String,
}

#[derive(Default)]
struct MockState {
    by_target: HashMap<String, MockResponse>,
    by_id: HashMap<String, MockResponse>,
    default: Option<MockResponse>,
    observed: Vec<PromptSummary>,
}

/// In-process driver for `VETTERD_NOTIFIER=mock`.
///
/// Construct one *before* spawning the daemon so the listener is
/// already bound when the daemon's `MockNotifier` tries to connect.
/// The accept thread runs until the listener is dropped (which
/// happens when `MockUi` is dropped or the tempdir is cleaned up at
/// test end).
///
/// Tests configure responses via [`set_default`], [`expect_target`],
/// [`expect_id`]; observed summaries are available via
/// [`wait_for_observed`].
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
            .expect("spawn mock ui thread");

        Self { socket_path, state }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn set_default(&self, response: MockResponse) {
        self.state.lock().expect("mock state").default = Some(response);
    }

    pub fn expect_target(&self, target: &str, response: MockResponse) {
        self.state
            .lock()
            .expect("mock state")
            .by_target
            .insert(target.to_string(), response);
    }

    pub fn expect_id(&self, id: &str, response: MockResponse) {
        self.state
            .lock()
            .expect("mock state")
            .by_id
            .insert(id.to_string(), response);
    }

    pub fn observed(&self) -> Vec<PromptSummary> {
        self.state.lock().expect("mock state").observed.clone()
    }

    pub fn observed_count(&self) -> usize {
        self.state.lock().expect("mock state").observed.len()
    }

    /// Block (with a deadline) until the mock has seen at least
    /// `min_count` prompts.
    pub fn wait_for_observed(&self, min_count: usize, timeout: Duration) -> Vec<PromptSummary> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = self.observed();
            if now.len() >= min_count {
                return now;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out after {:?} waiting for {} prompts; observed {}",
                    timeout,
                    min_count,
                    now.len()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Wire response written back to the daemon's `MockNotifier`. Local
/// type rather than reusing the daemon's so the test crate doesn't
/// need to import internal types.
#[derive(Debug, Serialize)]
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
                    if let Err(e) = handle_one(stream, state) {
                        eprintln!("mock ui worker: {e}");
                    }
                });
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return,
        }
    }
}

fn handle_one(stream: UnixStream, state: Arc<Mutex<MockState>>) -> std::io::Result<()> {
    // macOS inherits O_NONBLOCK from a nonblocking listener onto the
    // accepted fd. Leaving it set turns an early read_line into
    // WouldBlock; we then drop the stream and the daemon's write hits
    // Broken pipe. Linux does not inherit the flag, which is why this
    // only flakes on macos-latest.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(());
    }
    let summary: PromptSummary = serde_json::from_str(line.trim_end())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let response = {
        let mut g = state.lock().expect("mock state");
        let r = g
            .by_id
            .get(&summary.id)
            .cloned()
            .or_else(|| g.by_target.get(&summary.primary_target).cloned())
            .or_else(|| g.default.clone())
            .unwrap_or_else(|| MockResponse::deny("mock ui default (no rule configured)"));
        g.observed.push(summary.clone());
        r
    };
    let resp = WireResponse {
        decision: match response.decision {
            WireDecision::Allow => "allow",
            WireDecision::Deny => "deny",
            WireDecision::AllowOnce => "allow",
        },
        reason: &response.reason,
    };
    let mut bytes = serde_json::to_vec(&resp).map_err(|e| std::io::Error::other(e.to_string()))?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    // Some MockNotifier callers read the response with read_line; if
    // we close immediately the read may race. Sleeping a moment before
    // the OS-level FIN is overkill, but a flush + drop on the writer
    // handle is enough.
    Ok(())
}

/// A spawned `vetterd` subprocess plus the `MockUi` driving its
/// notifier. Drops in the right order: SIGTERM the daemon first, then
/// drop the listener.
pub struct Daemon {
    child: Child,
    pub socket: PathBuf,
    pub audit: PathBuf,
    pub pidfile: PathBuf,
    pub ui: MockUi,
    _scratch: TempDir,
}

impl Daemon {
    /// PID of the spawned `vetterd` process. Used by tests that want
    /// to cross-check kernel-reported peer pid / lock holder pid
    /// against the daemon's own identity.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Daemon {
    /// Spawn `vetterd` with `allowlist_yaml` and the standard mock
    /// notifier. The default mock response is "deny"; tests that
    /// want to approve a prompt configure
    /// [`MockUi::set_default`] / [`MockUi::expect_target`] before
    /// triggering the request.
    pub fn spawn(allowlist_yaml: &str) -> Self {
        Self::spawn_with_env(allowlist_yaml, &[])
    }

    /// Spawn the daemon with an additional batch of (key, value) env
    /// pairs layered on top of the standard test scaffolding. Used
    /// by tests that want to twiddle daemon-side knobs like
    /// `VETTERD_MAX_INFLIGHT` without forking a parallel
    /// `Command::new` boilerplate.
    pub fn spawn_with_env(allowlist_yaml: &str, extra_env: &[(&str, &str)]) -> Self {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket = scratch.path().join("vetter.sock");
        let audit = scratch.path().join("audit.log");
        // Co-located pidfile, matching `default_pidfile_path` when
        // VETTERD_PIDFILE is unset. Surfaced on `Daemon` so tests can
        // probe `pidfile::read_locker_pid` without re-deriving the
        // path.
        let pidfile = scratch.path().join("vetter.pid");
        let allow_path = scratch.path().join("allowlist.yaml");
        std::fs::write(&allow_path, allowlist_yaml).expect("write allowlist");

        let ui_socket = scratch.path().join("notifier.sock");
        let ui = MockUi::new(ui_socket.clone());

        let bin = assert_cmd::cargo_bin!("vetterd");
        let mut cmd = Command::new(bin);
        cmd.env("VETTERD_SOCKET", &socket)
            .env("VETTER_AUDIT_LOG", &audit)
            .env("VETTER_ALLOWLIST", &allow_path)
            .env_remove("VETTER_ALLOWLIST_OVERRIDE")
            .env("VETTERD_NOTIFIER", "mock")
            .env("VETTERD_NOTIFIER_SOCKET", &ui_socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn vetterd");
        wait_for_socket(&socket, Duration::from_secs(5));
        Self {
            child,
            socket,
            audit,
            pidfile,
            ui,
            _scratch: scratch,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // SIGTERM via libc; signal-hook installed the handler in the
        // daemon's own thread.
        unsafe {
            libc_kill(self.child.id() as i32, 15);
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

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

unsafe fn libc_kill(pid: i32, sig: i32) {
    let _ = kill(pid, sig);
}

pub fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon socket did not appear at {}", path.display());
}

/// Build a v2 [`VetRequest`] from the wrapped command's argv. The
/// daemon does its own parse, so callers no longer pass a pre-built
/// `ParsedCommand` — they hand over the same `argv` they would type
/// into a shell.
pub fn make_req<I, S>(argv: I, force_prompt: bool) -> VetRequest
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    VetRequest {
        v: PROTOCOL_VERSION,
        id: new_request_id(),
        cwd: None,
        agent_hint: None,
        argv: argv.into_iter().map(Into::into).collect(),
        force_prompt,
    }
}

pub fn curl_get(url: &str) -> Vec<String> {
    vec!["curl".into(), url.into()]
}

pub fn round_trip(socket: &Path, req: &VetRequest) -> VetDecision {
    let mut s = UnixStream::connect(socket).expect("connect");
    write_frame(&mut s, req).expect("write frame");
    s.flush().ok();
    read_decision(&mut s, &req.id).expect("read decision")
}

/// Drain `n` bytes from `stream` (best-effort) so test garbage probes
/// can confirm the daemon stays alive afterwards.
pub fn drain_to_eof(mut stream: UnixStream) {
    let mut sink = [0u8; 256];
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    while let Ok(n) = stream.read(&mut sink) {
        if n == 0 {
            break;
        }
    }
}
