//! Integration coverage for the Phase-5 admin handlers
//! ([`MgmtRequest::AddRule`] / [`MgmtRequest::AddKnownHost`] /
//! [`MgmtRequest::SuggestionsFor`]).
//!
//! Spawns the real `vetterd` binary with `VETTERD_NOTIFIER=noop` so
//! prompt-class requests park indefinitely. We then drive the admin
//! socket end-to-end:
//!
//! 1. Submit a curl request → noop notifier parks it.
//! 2. Send `AddRule` over the admin socket with a covering pattern.
//! 3. Assert `auto_approved_ids` includes our request, the worker
//!    reads `Allow` off the wire, the YAML on disk now contains the
//!    new rule, and the audit-log row carries the auto-approve
//!    reason.
//!
//! `AddKnownHost` is covered with the symmetric assertion: the
//! pending list still has the request after the call, but its
//! signal set no longer carries `UnknownHost` (and `host_known`
//! flips to `true`) — known-hosts only affect the UI signal, not
//! the policy decision, so auto-approve must NOT fire.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vetter_core::known_hosts::KnownHostEntry;
use vetter_core::matcher::rule::{HostPattern, HttpClause, Rule, RuleWhen, UrlClause};
use vetter_core::wire::{
    new_request_id, read_decision, read_frame, write_frame, MgmtRequest, MgmtResponse, VetDecision,
    VetRequest, WireDecision, WireError, WireScope, PROTOCOL_VERSION,
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

struct DaemonHandle {
    child: std::process::Child,
    socket: std::path::PathBuf,
    admin_socket: std::path::PathBuf,
    audit_path: std::path::PathBuf,
    allow_path: std::path::PathBuf,
    home: std::path::PathBuf,
    _scratch: TempDir,
}

impl DaemonHandle {
    fn spawn() -> Self {
        let scratch = TempDir::new().unwrap();
        let socket = scratch.path().join("vetter.sock");
        let admin_socket = scratch.path().join("vetter-admin.sock");
        let audit_path = scratch.path().join("audit.log");
        let allow_path = scratch.path().join("allowlist.yaml");
        std::fs::write(&allow_path, EMPTY_ALLOWLIST).unwrap();
        // Sandbox `$HOME` so `add_known_host` writes inside our
        // tempdir's `~/.vet/known-hosts.yaml` rather than the
        // tester's real home. The allowlist override path is wired
        // directly into Context but known-hosts has no analogous
        // env knob — the host-write path goes through
        // `user_known_hosts_path()` which honours `$HOME`.
        let home = scratch.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let child = Command::new(assert_cmd::cargo_bin!("vetterd"))
            .env("VETTERD_SOCKET", &socket)
            .env("VETTER_AUDIT_LOG", &audit_path)
            .env("VETTER_ALLOWLIST", &allow_path)
            .env("VETTERD_NOTIFIER", "noop")
            .env("HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn vetterd");

        wait_for_socket(&socket, Duration::from_secs(5));
        wait_for_socket(&admin_socket, Duration::from_secs(2));

        Self {
            child,
            socket,
            admin_socket,
            audit_path,
            allow_path,
            home,
            _scratch: scratch,
        }
    }

    fn kill(mut self) {
        unsafe { libc_kill(self.child.id() as i32, 15) };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() >= deadline => {
                    self.child.kill().ok();
                    self.child.wait().ok();
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return,
            }
        }
    }
}

fn submit_pending_request(
    socket: &Path,
    url: &str,
) -> (
    String,
    std::thread::JoinHandle<Result<VetDecision, WireError>>,
) {
    let req = VetRequest {
        v: PROTOCOL_VERSION,
        id: new_request_id(),
        cwd: None,
        agent_hint: None,
        argv: vec!["curl".into(), url.into()],
        force_prompt: false,
    };
    let id = req.id.clone();
    let socket_clone = socket.to_path_buf();
    let handle = std::thread::spawn(move || {
        let mut s = UnixStream::connect(&socket_clone).expect("connect main socket");
        write_frame(&mut s, &req).expect("write VetRequest");
        read_decision(&mut s, &req.id)
    });
    (id, handle)
}

fn wait_pending(admin: &Path, expected_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let resp = send_mgmt(admin, MgmtRequest::ListPending);
        if let MgmtResponse::PendingList { items } = resp {
            if items.iter().any(|i| i.id == expected_id) {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!("pending request {expected_id} did not appear");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn covering_rule_for(host: &str) -> Rule {
    Rule {
        id: String::new(),
        command: Some("curl".into()),
        when: RuleWhen {
            http: Some(HttpClause {
                method: None,
                url: Some(UrlClause {
                    scheme: None,
                    host: Some(HostPattern::One(host.into())),
                    port: None,
                    path: Some("/**".into()),
                }),
                headers_allow: None,
                no_body: None,
                query: None,
            }),
            file_write: None,
            file_read: None,
        },
        note: Some("auto-test rule".into()),
        created_by: None,
        created_at: None,
    }
}

#[test]
fn add_rule_auto_approves_matching_pending() {
    let daemon = DaemonHandle::spawn();
    let (req_id, worker) =
        submit_pending_request(&daemon.socket, "https://api.example.test/v1/data");
    wait_pending(&daemon.admin_socket, &req_id);

    let rule = covering_rule_for("api.example.test");
    let resp = send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::AddRule {
            scope: WireScope::User,
            rule: Box::new(rule),
        },
    );
    let (added_id, auto_ids) = match resp {
        MgmtResponse::RuleAdded {
            id,
            scope,
            auto_approved_ids,
        } => {
            assert_eq!(scope, WireScope::User);
            (id, auto_approved_ids)
        }
        other => panic!("unexpected admin response: {other:?}"),
    };
    assert!(added_id.starts_with("auto-"), "id={added_id}");
    assert!(
        auto_ids.contains(&req_id),
        "expected req {req_id} in auto_approved_ids {auto_ids:?}"
    );

    // Worker must observe Allow on the wire.
    let decision = worker
        .join()
        .expect("worker thread panicked")
        .expect("worker io ok");
    assert_eq!(decision.decision, WireDecision::Allow);
    assert!(
        decision
            .reason
            .contains("auto-approved by newly added rule"),
        "reason was {:?}",
        decision.reason
    );

    // YAML on disk now carries the new rule.
    let yaml = std::fs::read_to_string(&daemon.allow_path).unwrap();
    assert!(
        yaml.contains(&added_id),
        "allowlist did not pick up new id {added_id}: {yaml}"
    );
    assert!(
        yaml.contains("api.example.test"),
        "allowlist missing host: {yaml}"
    );

    // Audit log carries the auto-approve reason for our request id.
    let audit = std::fs::read_to_string(&daemon.audit_path).unwrap();
    let row = audit
        .lines()
        .find(|line| line.contains(&req_id))
        .expect("no audit row for request id");
    assert!(
        row.contains("auto-approved by newly added rule"),
        "audit row missing reason: {row}"
    );

    daemon.kill();
}

#[test]
fn add_rule_with_non_covering_pattern_leaves_pending_untouched() {
    let daemon = DaemonHandle::spawn();
    let (req_id, worker) =
        submit_pending_request(&daemon.socket, "https://api.example.test/v1/data");
    wait_pending(&daemon.admin_socket, &req_id);

    // Rule for a different host — should not match.
    let resp = send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::AddRule {
            scope: WireScope::User,
            rule: Box::new(covering_rule_for("api.different-host.test")),
        },
    );
    let auto_ids = match resp {
        MgmtResponse::RuleAdded {
            auto_approved_ids, ..
        } => auto_approved_ids,
        other => panic!("unexpected admin response: {other:?}"),
    };
    assert!(
        auto_ids.is_empty(),
        "non-covering rule should not auto-approve anything, got {auto_ids:?}"
    );

    // Pending list still contains our request.
    let resp = send_mgmt(&daemon.admin_socket, MgmtRequest::ListPending);
    if let MgmtResponse::PendingList { items } = resp {
        assert!(
            items.iter().any(|i| i.id == req_id),
            "pending request {req_id} disappeared after non-covering AddRule"
        );
    } else {
        panic!("unexpected pending list response");
    }

    daemon.kill();
    let _ = worker.join();
}

#[test]
fn add_known_host_does_not_auto_approve_but_flips_signals() {
    let daemon = DaemonHandle::spawn();
    let (req_id, worker) =
        submit_pending_request(&daemon.socket, "https://api.unknown.test/v1/data");
    wait_pending(&daemon.admin_socket, &req_id);

    // Fetch suggestions first to confirm the host is currently unknown
    // (and therefore eligible for the wildcard tier).
    let resp = send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::SuggestionsFor { id: req_id.clone() },
    );
    let host_suggestions = match resp {
        MgmtResponse::Suggestions { known_host, .. } => known_host,
        other => panic!("unexpected admin response: {other:?}"),
    };
    assert!(
        !host_suggestions.is_empty(),
        "expected at least one host suggestion for unknown host"
    );

    let resp = send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::AddKnownHost {
            scope: WireScope::User,
            entry: KnownHostEntry {
                pattern: "api.unknown.test".into(),
                note: Some("trusted via test".into()),
            },
        },
    );
    match resp {
        MgmtResponse::KnownHostAdded { pattern, scope } => {
            assert_eq!(scope, WireScope::User);
            assert_eq!(pattern, "api.unknown.test");
        }
        other => panic!("unexpected admin response: {other:?}"),
    }

    // Pending request must remain pending (known-hosts don't auto-approve).
    let resp = send_mgmt(&daemon.admin_socket, MgmtRequest::ListPending);
    let still_pending = matches!(
        resp,
        MgmtResponse::PendingList { ref items } if items.iter().any(|i| i.id == req_id)
    );
    assert!(
        still_pending,
        "AddKnownHost should not auto-approve; got {resp:?}"
    );

    // Re-fetch suggestions: known-host wildcard / exact tiers should
    // now be empty (host is trusted).
    let resp = send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::SuggestionsFor { id: req_id.clone() },
    );
    if let MgmtResponse::Suggestions { known_host, .. } = resp {
        assert!(
            known_host.is_empty(),
            "host suggestions should be empty after trusting; got {known_host:?}"
        );
    } else {
        panic!("expected Suggestions");
    }

    // Known-hosts file lives under HOME/.vet/known-hosts.yaml.
    let kh = daemon.home.join(".vet").join("known-hosts.yaml");
    let yaml = std::fs::read_to_string(&kh).expect("known-hosts file written");
    assert!(
        yaml.contains("api.unknown.test"),
        "known-hosts file missing pattern: {yaml}"
    );

    daemon.kill();
    let _ = worker.join();
}

/// Phase 5.1: the popover's "Revoke rule" button hits the daemon over
/// the admin socket via [`MgmtRequest::RemoveRule`]. End-to-end check
/// that an `AddRule` followed by a `RemoveRule` round-trips through
/// the wire, the YAML on disk loses the rule, and a request that
/// would have been auto-allowed now parks as a prompt-class
/// request again.
#[test]
fn remove_rule_drops_persisted_rule_and_reverts_auto_allow() {
    let daemon = DaemonHandle::spawn();

    // Add a covering rule first so the daemon has something to
    // remove. We don't drive a pending request through it — that
    // path is covered by `add_rule_auto_approves_matching_pending`;
    // here we want the rule on disk + in the live store before the
    // remove call.
    let added_id = match send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::AddRule {
            scope: WireScope::User,
            rule: Box::new(covering_rule_for("api.example.test")),
        },
    ) {
        MgmtResponse::RuleAdded { id, .. } => id,
        other => panic!("unexpected AddRule response: {other:?}"),
    };

    // Sanity: the rule is live in the YAML file before remove.
    let yaml_before = std::fs::read_to_string(&daemon.allow_path).unwrap();
    assert!(
        yaml_before.contains(&added_id),
        "allowlist YAML missing rule id before remove: {yaml_before}"
    );

    // Send the RemoveRule admin call.
    let resp = send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::RemoveRule {
            scope: WireScope::User,
            id: added_id.clone(),
        },
    );
    match resp {
        MgmtResponse::RuleRemoved { id, scope } => {
            assert_eq!(scope, WireScope::User);
            assert_eq!(id, added_id);
        }
        other => panic!("unexpected RemoveRule response: {other:?}"),
    }

    // YAML on disk no longer contains the rule.
    let yaml_after = std::fs::read_to_string(&daemon.allow_path).unwrap();
    assert!(
        !yaml_after.contains(&added_id),
        "allowlist YAML still contains rule id after remove: {yaml_after}"
    );

    // A new request that would have been auto-allowed by the rule
    // now parks as a prompt-class request again — proves the
    // in-memory store reloaded too, not just the file.
    let (req_id, worker) =
        submit_pending_request(&daemon.socket, "https://api.example.test/v1/data");
    wait_pending(&daemon.admin_socket, &req_id);

    daemon.kill();
    // The submit thread is parked on a noop-notifier prompt that
    // never resolves; killing the daemon wakes it with a connection
    // error which is fine for our purposes.
    let _ = worker.join();
}

/// Sending RemoveRule with a nonexistent id surfaces an Error
/// response instead of silently succeeding — the popover alert
/// needs *some* failure string to render.
#[test]
fn remove_rule_unknown_id_returns_error() {
    let daemon = DaemonHandle::spawn();
    let resp = send_mgmt(
        &daemon.admin_socket,
        MgmtRequest::RemoveRule {
            scope: WireScope::User,
            id: "no-such-rule".into(),
        },
    );
    match resp {
        MgmtResponse::Error { message } => {
            assert!(
                !message.is_empty(),
                "Error response must carry a non-empty message"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
    daemon.kill();
}
