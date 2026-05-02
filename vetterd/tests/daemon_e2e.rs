//! End-to-end tests that spawn the real `vetterd` binary on a
//! tempdir socket and exercise the wire protocol from a `vet`-shaped
//! client. Each test gets its own scratch dir; env (`VETTERD_SOCKET`,
//! `VETTER_AUDIT_LOG`, `VETTER_ALLOWLIST`) is set per-spawn so tests
//! parallelise without stomping on each other.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vetter_core::wire::{
    new_request_id, read_decision, write_frame, VetRequest, WireDecision, PROTOCOL_VERSION,
};
use vetterd::AuditEntry;

struct Daemon {
    child: Child,
    pub socket: PathBuf,
    pub audit: PathBuf,
    _scratch: TempDir,
}

impl Daemon {
    fn spawn(allowlist_yaml: &str) -> Self {
        let scratch = tempfile::tempdir().expect("tempdir");
        let socket = scratch.path().join("vetter.sock");
        let audit = scratch.path().join("audit.log");
        let allow_path = scratch.path().join("allowlist.yaml");
        std::fs::write(&allow_path, allowlist_yaml).expect("write allowlist");
        let bin = assert_cmd::cargo_bin!("vetterd");
        let child = Command::new(bin)
            .env("VETTERD_SOCKET", &socket)
            .env("VETTER_AUDIT_LOG", &audit)
            .env("VETTER_ALLOWLIST", &allow_path)
            .env_remove("VETTER_ALLOWLIST_OVERRIDE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn vetterd");
        wait_for_socket(&socket, Duration::from_secs(5));
        Self {
            child,
            socket,
            audit,
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

/// Build a v2 [`VetRequest`] from the wrapped command's argv. The
/// daemon does its own parse, so callers no longer pass a pre-built
/// `ParsedCommand` — they hand over the same `argv` they would type
/// into a shell.
fn make_req<I, S>(argv: I, force_prompt: bool) -> VetRequest
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

fn curl_get(url: &str) -> Vec<String> {
    vec!["curl".into(), url.into()]
}

fn round_trip(socket: &Path, req: &VetRequest) -> vetter_core::wire::VetDecision {
    let mut s = UnixStream::connect(socket).expect("connect");
    write_frame(&mut s, req).expect("write frame");
    s.flush().ok();
    read_decision(&mut s, &req.id).expect("read decision")
}

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
deny:
  - id: blocked-host
    when:
      http:
        method: [GET]
        url:
          scheme: https
          host: blocked.test
        headers_allow: ["*"]
"#;

#[test]
fn allow_path_returns_allow_with_rule_id_in_reason() {
    let d = Daemon::spawn(ALLOWLIST);
    let req = make_req(curl_get("https://example.test/"), false);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(dec.decision, WireDecision::Allow);
    assert!(dec.reason.contains("example-get"), "{}", dec.reason);
    assert_eq!(dec.id, req.id);
}

#[test]
fn deny_path_returns_deny_with_denylist_scope_in_reason() {
    let d = Daemon::spawn(ALLOWLIST);
    let req = make_req(curl_get("https://blocked.test/"), false);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(dec.decision, WireDecision::Deny);
    assert!(dec.reason.contains("blocked-host"), "{}", dec.reason);
    assert!(dec.reason.contains("denylist"), "{}", dec.reason);
}

#[test]
fn prompt_class_falls_back_to_stub_deny() {
    let d = Daemon::spawn(ALLOWLIST);
    let req = make_req(curl_get("https://unmatched.test/"), false);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(dec.decision, WireDecision::Deny);
    assert!(dec.reason.contains("no UI yet"), "{}", dec.reason);
}

#[test]
fn force_prompt_overrides_existing_allow_rule() {
    let d = Daemon::spawn(ALLOWLIST);
    let req = make_req(curl_get("https://example.test/"), true);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(dec.decision, WireDecision::Deny);
    assert!(dec.reason.contains("no UI yet"), "{}", dec.reason);
}

#[test]
fn concurrent_clients_get_independent_correlated_decisions() {
    let d = Daemon::spawn(ALLOWLIST);
    let socket = d.socket.clone();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let socket = socket.clone();
            std::thread::spawn(move || {
                let url = if i % 2 == 0 {
                    "https://example.test/"
                } else {
                    "https://blocked.test/"
                };
                let req = make_req(curl_get(url), false);
                let expected_id = req.id.clone();
                let dec = round_trip(&socket, &req);
                (i, expected_id, dec)
            })
        })
        .collect();
    for h in handles {
        let (i, expected_id, dec) = h.join().expect("worker join");
        assert_eq!(dec.id, expected_id, "id mismatch for worker {i}");
        if i % 2 == 0 {
            assert_eq!(dec.decision, WireDecision::Allow, "worker {i}");
        } else {
            assert_eq!(dec.decision, WireDecision::Deny, "worker {i}");
        }
    }
}

#[test]
fn audit_log_records_one_entry_per_request_with_decision() {
    let d = Daemon::spawn(ALLOWLIST);
    let req_a = make_req(curl_get("https://example.test/"), false);
    let req_b = make_req(curl_get("https://blocked.test/"), false);
    let dec_a = round_trip(&d.socket, &req_a);
    let dec_b = round_trip(&d.socket, &req_b);

    // Audit append happens before write_frame returns the decision,
    // so the log line is durable by the time we read here.
    let body = std::fs::read_to_string(&d.audit).expect("audit log read");
    assert!(body.ends_with('\n'), "missing terminator: {body:?}");
    let lines: Vec<_> = body.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 entries: {body}");
    let by_id = lines
        .iter()
        .map(|l| serde_json::from_str::<AuditEntry>(l).expect("parse"))
        .map(|e| (e.id.clone(), e))
        .collect::<std::collections::HashMap<_, _>>();
    let a = &by_id[&dec_a.id];
    assert_eq!(a.decision, WireDecision::Allow);
    assert!(a.reason.contains("example-get"), "{:?}", a);
    // Audit `command` is the daemon-derived parser name, not whatever
    // the client claimed. T2 regression check: an attacker submitting
    // `argv: ["curl", ...]` cannot scribble a different command into
    // the log.
    assert_eq!(a.command, "curl");
    let b = &by_id[&dec_b.id];
    assert_eq!(b.decision, WireDecision::Deny);
    assert!(b.reason.contains("blocked-host"), "{:?}", b);
    assert_eq!(b.command, "curl");
}

/// T2 regression: with v2 wire, the daemon parses argv itself, so a
/// client cannot submit `argv: ["curl", "https://blocked.test/"]` and
/// have it allowed by lying about parsed effects. The wire format
/// physically has nowhere to put the lie any more, but we also assert
/// here that a request whose argv hits a denylist rule is denied —
/// proving the daemon did the parse and reached the matcher with
/// effects derived from the real argv.
#[test]
fn daemon_reparses_argv_so_client_cannot_forge_effects() {
    let d = Daemon::spawn(ALLOWLIST);
    let req = make_req(curl_get("https://blocked.test/"), false);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(dec.decision, WireDecision::Deny);
    assert!(dec.reason.contains("blocked-host"), "{}", dec.reason);
}

/// T2 regression: a request whose argv the daemon cannot parse fails
/// closed (deny) with a `parse failed` reason. Earlier the daemon
/// would have happily evaluated whatever `parsed` field the client
/// supplied; v2 requires the daemon to derive the effects itself.
#[test]
fn unparseable_argv_is_denied_with_parse_failed_reason() {
    let d = Daemon::spawn(ALLOWLIST);
    // Curl with no URL at all is a `MissingArgument("URL")` — a real
    // ParseError that the daemon must surface as deny.
    let req = make_req(["curl"], false);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(dec.decision, WireDecision::Deny);
    assert!(dec.reason.contains("parse failed"), "{}", dec.reason);
    assert!(dec.reason.contains("URL"), "{}", dec.reason);
}

/// T2 regression: an argv whose `argv[0]` resolves to no registered
/// parser is also denied (the daemon would otherwise have to fall
/// back to client-supplied effects, which v2 deliberately doesn't
/// have).
#[test]
fn unknown_command_is_denied() {
    let d = Daemon::spawn(ALLOWLIST);
    let req = make_req(["totally-not-a-real-binary", "--help"], false);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(dec.decision, WireDecision::Deny);
    assert!(dec.reason.contains("parse failed"), "{}", dec.reason);
    assert!(dec.reason.contains("no parser"), "{}", dec.reason);
}

#[test]
fn connection_refused_after_daemon_killed() {
    let d = Daemon::spawn(ALLOWLIST);
    let socket = d.socket.clone();
    drop(d); // SIGTERMs and waits
             // After Drop the daemon removes the socket file; connect should fail.
    let err = UnixStream::connect(&socket).err();
    assert!(
        err.is_some(),
        "expected connect error after shutdown, got Ok"
    );
}

#[test]
fn version_mismatch_request_is_rejected_by_daemon() {
    let d = Daemon::spawn(ALLOWLIST);
    let mut req = make_req(curl_get("https://example.test/"), false);
    req.v = 99;
    let mut s = UnixStream::connect(&d.socket).expect("connect");
    write_frame(&mut s, &req).expect("write frame");
    // Daemon prints to stderr and closes the connection; reading a
    // decision should fail with an io / framing error, not panic.
    let res = read_decision(&mut s, &req.id);
    assert!(res.is_err(), "expected error on bad version, got {res:?}");
}

/// §6.3 — Feed a variety of malformed byte sequences to the daemon socket.
///
/// For each probe: connect, send garbage, drain until EOF or error.
/// After all probes, send a valid framed request and assert a well-formed
/// decision comes back — this proves the daemon is still alive and accepting
/// connections, i.e. no panic / wedge occurred.
#[test]
fn random_bytes_do_not_crash_daemon() {
    use std::io::Read as _;

    let d = Daemon::spawn(ALLOWLIST);

    let garbage_probes: &[&[u8]] = &[
        // Empty send (immediate EOF from writer's side)
        b"",
        // Truncated length prefix (only 2 of 4 bytes)
        &[0x00, 0x01],
        // Length prefix says 5 MiB body — daemon must reject before allocating
        &[0xFF, 0xFF, 0xFF, 0xFF],
        // Well-formed 4-byte length (12) but body is pure garbage
        &[
            0x00, 0x00, 0x00, 0x0C, 0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00,
            0x00, 0x00,
        ],
        // Looks like valid JSON length but body is not JSON
        &[0x00, 0x00, 0x00, 0x05, b'n', b'u', b'l', b'l', b'!'],
        // Random-ish printable bytes with no framing at all
        b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n",
        // Null bytes
        &[0x00; 16],
        // Single byte
        &[0x42],
    ];

    let timeout = std::time::Duration::from_secs(1);

    for (i, probe) in garbage_probes.iter().enumerate() {
        let mut s = UnixStream::connect(&d.socket)
            .unwrap_or_else(|e| panic!("probe {i}: connect failed: {e}"));
        s.set_write_timeout(Some(timeout))
            .expect("set_write_timeout");
        s.set_read_timeout(Some(timeout)).expect("set_read_timeout");

        if !probe.is_empty() {
            // Best-effort send; the daemon may close before we finish writing.
            let _ = s.write_all(probe);
        }
        // Drain until EOF or error — we only care that this completes and the
        // daemon doesn't panic.
        let mut sink = [0u8; 256];
        loop {
            match s.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }

    // Daemon must still be alive: a valid round-trip succeeds.
    let req = make_req(curl_get("https://example.test/"), false);
    let dec = round_trip(&d.socket, &req);
    assert_eq!(
        dec.decision,
        WireDecision::Allow,
        "daemon should still be functional after garbage probes"
    );
}
