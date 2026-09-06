//! Integration tests for the admin socket (`vet daemon list` IPC).
//!
//! Uses `VETTERD_NOTIFIER=noop` so prompt-class requests park in the
//! pending queue indefinitely — no mock UI race to contend with. The
//! daemon is SIGTERM'd at the end, which triggers `cancel_all()` and
//! unblocks the parked connection workers with a deny.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vetter_core::wire::{
    new_request_id, read_decision, read_frame, write_frame, MgmtRequest, MgmtResponse, VetDecision,
    VetRequest, WireDecision, WireError, PROTOCOL_VERSION,
};

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
    panic!("socket did not appear at {}", path.display());
}

fn send_mgmt(path: &Path, req: MgmtRequest) -> MgmtResponse {
    let mut stream = UnixStream::connect(path).expect("connect to admin socket");
    write_frame(&mut stream, &req).expect("write MgmtRequest");
    read_frame::<_, MgmtResponse>(&mut stream).expect("read MgmtResponse")
}

const EMPTY_ALLOWLIST: &str = "rules: []\ndeny: []\n";

fn spawn_noop_daemon(
    scratch: &TempDir,
) -> (std::process::Child, std::path::PathBuf, std::path::PathBuf) {
    let socket = scratch.path().join("vetter.sock");
    let admin_socket = scratch.path().join("vetter-admin.sock");
    let audit = scratch.path().join("audit.log");
    let allow = scratch.path().join("allowlist.yaml");
    std::fs::write(&allow, EMPTY_ALLOWLIST).unwrap();

    let child = Command::new(assert_cmd::cargo_bin!("vetterd"))
        .env("VETTERD_SOCKET", &socket)
        .env("VETTER_AUDIT_LOG", &audit)
        .env("VETTER_ALLOWLIST", &allow)
        .env("VETTERD_NOTIFIER", "noop")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn vetterd");

    wait_for_socket(&socket, Duration::from_secs(5));
    wait_for_socket(&admin_socket, Duration::from_secs(2));

    (child, socket, admin_socket)
}

/// The audit log `spawn_noop_daemon` points the daemon at. Derived
/// rather than returned so the existing three-tuple call sites don't
/// have to change.
fn audit_path(scratch: &TempDir) -> std::path::PathBuf {
    scratch.path().join("audit.log")
}

/// Park one prompt-class `curl` request on the main socket.
///
/// Returns the request's ULID plus the join handle for the connection
/// worker, which stays blocked until something resolves the request:
/// the noop notifier never does, so these tests are the only thing
/// that can unblock it. Join the handle *after* resolving to read the
/// decision the daemon wrote back.
type ParkedWorker = std::thread::JoinHandle<Result<VetDecision, WireError>>;

fn park_request(socket: &Path, url: &str) -> (String, ParkedWorker) {
    let req = VetRequest {
        v: PROTOCOL_VERSION,
        id: new_request_id(),
        cwd: None,
        agent_hint: None,
        argv: vec!["curl".into(), url.into()],
        force_prompt: false,
    };
    let id = req.id.clone();
    let socket = socket.to_path_buf();
    let handle = std::thread::spawn(move || {
        let mut s = UnixStream::connect(&socket).expect("connect main socket");
        write_frame(&mut s, &req).expect("write VetRequest");
        read_decision(&mut s, &req.id)
    });
    (id, handle)
}

/// Poll `ListPending` until exactly `n` requests are parked. The
/// submit is asynchronous relative to the admin socket, so every
/// resolve test has to wait for the queue to settle first.
fn wait_for_pending(admin_socket: &Path, n: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let MgmtResponse::PendingList { items } =
            send_mgmt(admin_socket, MgmtRequest::ListPending)
        {
            if items.len() == n {
                return items.into_iter().map(|i| i.id).collect();
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {n} pending request(s)"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The audit row the daemon wrote for `id`, as raw JSON text.
fn audit_line_for(audit: &Path, id: &str) -> String {
    let body = std::fs::read_to_string(audit).expect("audit log readable");
    body.lines()
        .find(|l| l.contains(id))
        .unwrap_or_else(|| panic!("no audit line for id {id}: {body}"))
        .to_string()
}

fn resolve(admin_socket: &Path, id: &str, decision: WireDecision) -> MgmtResponse {
    send_mgmt(
        admin_socket,
        MgmtRequest::Resolve {
            id: id.to_string(),
            decision,
            reason: None,
        },
    )
}

fn sigterm_and_wait(mut child: std::process::Child) {
    unsafe { libc_kill(child.id() as i32, 15) };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() >= deadline => {
                child.kill().ok();
                child.wait().ok();
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return,
        }
    }
}

/// A freshly started daemon with no pending requests returns an empty list.
#[test]
fn admin_list_pending_empty_when_idle() {
    let scratch = TempDir::new().unwrap();
    let (child, _socket, admin_socket) = spawn_noop_daemon(&scratch);

    let resp = send_mgmt(&admin_socket, MgmtRequest::ListPending);
    match resp {
        MgmtResponse::PendingList { items } => {
            assert!(items.is_empty(), "expected empty list, got {:?}", items);
        }
        other => panic!("unexpected admin response: {:?}", other),
    }

    sigterm_and_wait(child);
}

/// Requests parked by the noop notifier appear in the pending list
/// with the correct fields, and disappear once the daemon shuts down.
#[test]
fn admin_list_pending_shows_parked_requests() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    // Submit a curl request in a background thread. The noop notifier
    // never resolves it so the worker blocks until cancel_all().
    let socket_clone = socket.clone();
    let req = VetRequest {
        v: PROTOCOL_VERSION,
        id: new_request_id(),
        cwd: None,
        agent_hint: None,
        argv: vec!["curl".into(), "https://api.example.test/v1/data".into()],
        force_prompt: false,
    };
    let req_id = req.id.clone();
    let handle = std::thread::spawn(move || {
        let mut s = UnixStream::connect(&socket_clone).expect("connect main socket");
        write_frame(&mut s, &req).expect("write VetRequest");
        read_decision(&mut s, &req.id)
    });

    // Poll the admin socket until the item appears (max 2 s).
    let items = {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let resp = send_mgmt(&admin_socket, MgmtRequest::ListPending);
            if let MgmtResponse::PendingList { items } = resp {
                if !items.is_empty() {
                    break items;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for pending item to appear"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    assert_eq!(items.len(), 1, "expected 1 pending item, got {:?}", items);
    let item = &items[0];
    assert_eq!(item.id, req_id, "id mismatch");
    assert_eq!(item.command, "curl");
    assert_eq!(item.primary_verb, "GET");
    assert!(
        item.primary_target.contains("api.example.test"),
        "unexpected primary_target: {}",
        item.primary_target
    );
    assert!(!item.force_prompt);

    // SIGTERM → cancel_all() → the pending worker wakes with a deny.
    sigterm_and_wait(child);

    // Drain the handle; don't assert on the decision payload because
    // the daemon may exit before the worker finishes writing its deny
    // response (acceptable race — no worker join at daemon shutdown).
    let _ = handle.join();
}

/// Multiple concurrent parked requests all appear in the list.
#[test]
fn admin_list_pending_multiple_requests() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let urls = [
        "https://a.example.test/",
        "https://b.example.test/",
        "https://c.example.test/",
    ];
    let mut req_ids = Vec::new();
    let mut handles = Vec::new();

    for url in &urls {
        let socket_clone = socket.clone();
        let req = VetRequest {
            v: PROTOCOL_VERSION,
            id: new_request_id(),
            cwd: None,
            agent_hint: None,
            argv: vec!["curl".into(), (*url).into()],
            force_prompt: false,
        };
        req_ids.push(req.id.clone());
        handles.push(std::thread::spawn(move || {
            let mut s = UnixStream::connect(&socket_clone).expect("connect");
            write_frame(&mut s, &req).expect("write");
            read_decision(&mut s, &req.id)
        }));
    }

    // Poll until all three requests appear in the queue.
    let items = {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let resp = send_mgmt(&admin_socket, MgmtRequest::ListPending);
            if let MgmtResponse::PendingList { items } = resp {
                if items.len() == urls.len() {
                    break items;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {} pending items",
                urls.len()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    assert_eq!(items.len(), urls.len());
    for id in &req_ids {
        assert!(
            items.iter().any(|i| &i.id == id),
            "id {id} missing from pending list: {:?}",
            items
        );
    }

    sigterm_and_wait(child);
    // Just drain the handles; don't assert on the decision payload
    // because the daemon may exit before all workers write their deny
    // responses (no worker join at daemon shutdown — acceptable race).
    for h in handles {
        let _ = h.join();
    }
}

// ── Phase 6a: headless resolve over the admin socket ────────────────

/// `Resolve { Allow }` unblocks the parked worker with an allow, and
/// the audit row records the promised admin-socket reason. This is
/// the whole point of Phase 6a: on a host with no approval UI, this
/// is the *only* thing that can unpark a prompt-class request.
#[test]
fn admin_resolve_approve_unblocks_worker_with_allow() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let (id, worker) = park_request(&socket, "https://approve.example.test/");
    wait_for_pending(&admin_socket, 1);

    match resolve(&admin_socket, &id, WireDecision::Allow) {
        MgmtResponse::Resolved {
            id: got,
            decision: WireDecision::Allow,
        } => assert_eq!(got, id, "daemon must echo the full resolved ULID"),
        other => panic!("unexpected admin response: {other:?}"),
    }

    let dec = worker.join().expect("worker thread").expect("decision");
    assert_eq!(dec.id, id);
    assert_eq!(dec.decision, WireDecision::Allow);
    assert_eq!(dec.reason, "approved via admin socket");

    let line = audit_line_for(&audit_path(&scratch), &id);
    let entry: serde_json::Value = serde_json::from_str(&line).expect("parse audit row");
    assert_eq!(entry["decision"], "allow", "{line}");
    assert_eq!(entry["reason"], "approved via admin socket", "{line}");

    // The queue is empty again — a resolved request must not linger.
    wait_for_pending(&admin_socket, 0);

    sigterm_and_wait(child);
}

/// `Resolve { Deny }` unblocks the parked worker with a deny (which
/// `vet` maps onto exit 77) and audits the matching reason.
#[test]
fn admin_resolve_reject_unblocks_worker_with_deny() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let (id, worker) = park_request(&socket, "https://reject.example.test/");
    wait_for_pending(&admin_socket, 1);

    match resolve(&admin_socket, &id, WireDecision::Deny) {
        MgmtResponse::Resolved {
            id: got,
            decision: WireDecision::Deny,
        } => assert_eq!(got, id),
        other => panic!("unexpected admin response: {other:?}"),
    }

    let dec = worker.join().expect("worker thread").expect("decision");
    assert_eq!(dec.decision, WireDecision::Deny);
    assert_eq!(dec.reason, "rejected via admin socket");

    let line = audit_line_for(&audit_path(&scratch), &id);
    let entry: serde_json::Value = serde_json::from_str(&line).expect("parse audit row");
    assert_eq!(entry["decision"], "deny", "{line}");
    assert_eq!(entry["reason"], "rejected via admin socket", "{line}");

    sigterm_and_wait(child);
}

/// An operator note rides along in the audit reason without
/// displacing the "via admin socket" provenance.
#[test]
fn admin_resolve_appends_operator_reason_to_audit() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let (id, worker) = park_request(&socket, "https://noted.example.test/");
    wait_for_pending(&admin_socket, 1);

    let resp = send_mgmt(
        &admin_socket,
        MgmtRequest::Resolve {
            id: id.clone(),
            decision: WireDecision::Deny,
            reason: Some("exfiltration risk".into()),
        },
    );
    assert!(
        matches!(resp, MgmtResponse::Resolved { .. }),
        "unexpected admin response: {resp:?}"
    );

    let dec = worker.join().expect("worker thread").expect("decision");
    assert_eq!(
        dec.reason, "rejected via admin socket: exfiltration risk",
        "operator note must be appended, not substituted"
    );

    let line = audit_line_for(&audit_path(&scratch), &id);
    assert!(line.contains("exfiltration risk"), "{line}");

    sigterm_and_wait(child);
}

/// A unique ULID prefix resolves — these ids are hand-typed, so
/// requiring all 26 characters would make the command unusable.
#[test]
fn admin_resolve_accepts_a_unique_id_prefix() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let (id, worker) = park_request(&socket, "https://prefix.example.test/");
    wait_for_pending(&admin_socket, 1);

    // Only one request is parked, so even a very short prefix is
    // unambiguous.
    let prefix = &id[..8];
    match resolve(&admin_socket, prefix, WireDecision::Allow) {
        MgmtResponse::Resolved { id: got, .. } => assert_eq!(
            got, id,
            "daemon must expand the prefix to the full ULID in its reply"
        ),
        other => panic!("unexpected admin response: {other:?}"),
    }

    let dec = worker.join().expect("worker thread").expect("decision");
    assert_eq!(dec.decision, WireDecision::Allow);

    sigterm_and_wait(child);
}

/// An ambiguous prefix is refused, and the error names every
/// candidate so the operator can retype with more characters.
#[test]
fn admin_resolve_rejects_an_ambiguous_id_prefix() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let (id_a, worker_a) = park_request(&socket, "https://amb-a.example.test/");
    let (id_b, worker_b) = park_request(&socket, "https://amb-b.example.test/");
    wait_for_pending(&admin_socket, 2);

    // Derive the shared prefix from the real ids rather than assuming
    // a length: two ULIDs minted in the same millisecond share their
    // 10-character timestamp and often more.
    let shared: String = id_a
        .chars()
        .zip(id_b.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a)
        .collect();
    assert!(
        !shared.is_empty(),
        "expected two same-millisecond ULIDs to share a prefix: {id_a} / {id_b}"
    );

    match resolve(&admin_socket, &shared, WireDecision::Allow) {
        MgmtResponse::Error { message } => {
            assert!(message.contains("ambiguous"), "{message}");
            assert!(message.contains(&id_a), "{message}");
            assert!(message.contains(&id_b), "{message}");
        }
        other => panic!("ambiguous prefix must not resolve: {other:?}"),
    }

    // Both requests are still parked — an ambiguous id must be a
    // no-op, not a coin flip.
    wait_for_pending(&admin_socket, 2);

    sigterm_and_wait(child);
    let _ = worker_a.join();
    let _ = worker_b.join();
}

/// Resolving the same id twice: the second call is an error, not a
/// silent success. A script that believes it unblocked a request it
/// never touched is worse than one that fails loudly.
#[test]
fn admin_resolve_twice_is_an_error_the_second_time() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let (id, worker) = park_request(&socket, "https://twice.example.test/");
    wait_for_pending(&admin_socket, 1);

    assert!(
        matches!(
            resolve(&admin_socket, &id, WireDecision::Allow),
            MgmtResponse::Resolved { .. }
        ),
        "first resolve should succeed"
    );
    let dec = worker.join().expect("worker thread").expect("decision");
    assert_eq!(dec.decision, WireDecision::Allow);

    match resolve(&admin_socket, &id, WireDecision::Deny) {
        MgmtResponse::Error { message } => {
            assert!(message.contains(&id), "{message}");
        }
        other => panic!("double-resolve must be an error: {other:?}"),
    }

    sigterm_and_wait(child);
}

/// An id that was never in the queue is an error.
#[test]
fn admin_resolve_unknown_id_is_an_error() {
    let scratch = TempDir::new().unwrap();
    let (child, _socket, admin_socket) = spawn_noop_daemon(&scratch);

    let stranger = new_request_id();
    match resolve(&admin_socket, &stranger, WireDecision::Allow) {
        MgmtResponse::Error { message } => {
            assert!(message.contains("no pending request"), "{message}");
            assert!(message.contains(&stranger), "{message}");
        }
        other => panic!("unknown id must be an error: {other:?}"),
    }

    sigterm_and_wait(child);
}

/// `AllowOnce` has no meaning on a socket that cannot scope the
/// "once", so the daemon refuses it rather than widening it to a
/// plain allow.
#[test]
fn admin_resolve_refuses_allow_once() {
    let scratch = TempDir::new().unwrap();
    let (child, socket, admin_socket) = spawn_noop_daemon(&scratch);

    let (id, worker) = park_request(&socket, "https://once.example.test/");
    wait_for_pending(&admin_socket, 1);

    match resolve(&admin_socket, &id, WireDecision::AllowOnce) {
        MgmtResponse::Error { message } => {
            assert!(message.contains("allow_once"), "{message}");
        }
        other => panic!("allow_once must be refused: {other:?}"),
    }

    // Still parked: a refused decision must not resolve anything.
    wait_for_pending(&admin_socket, 1);

    sigterm_and_wait(child);
    let _ = worker.join();
}
