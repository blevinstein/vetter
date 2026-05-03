//! Tests for [`crate`] root. Layout convention is described in
//! `AGENTS.md`.

use std::sync::RwLock;

use super::*;
use crate::notifier::NoopNotifier;
use crate::pending::PendingQueue;
use crate::testutil::tmpdir;
use vetter_core::wire::WireDecision;

#[test]
fn handle_connection_evaluates_and_responds() {
    let dir = tmpdir("vetterd-lib-test-");
    let sock = dir.path().join("test.sock");
    let audit_path = dir.path().join("audit.log");
    let allowlist = Arc::new(RwLock::new(load_default(None, None).unwrap()));
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let pending = Arc::new(PendingQueue::new());
    let notifier: Arc<dyn crate::notifier::Notifier> = Arc::new(NoopNotifier);
    let known_hosts = Arc::new(RwLock::new(load_known_hosts_default(None).unwrap()));
    let _ctx = Context {
        socket_path: sock,
        audit,
        allowlist,
        known_hosts,
        pending,
        notifier,
        allowlist_override: None,
    };
    // Smoke-only: full e2e is in vetterd/tests/daemon_e2e.rs.
    // Here we just confirm Context can be assembled.
    // Real wire round-trip requires a real socket pair; covered
    // by the integration test.
}

#[test]
fn timestamp_is_monotonic_ish() {
    let a = timestamp_iso8601();
    let b = timestamp_iso8601();
    assert!(a <= b, "{a} <= {b}");
}

#[test]
fn wire_decision_re_export_visible() {
    let d: WireDecision = WireDecision::Allow;
    assert_eq!(d, WireDecision::Allow);
}

// -- shell_quote / render_detail -------------------------------

#[test]
fn shell_quote_leaves_safe_args_alone() {
    assert_eq!(super::shell_quote("curl"), "curl");
    assert_eq!(
        super::shell_quote("https://example.com/v1/things"),
        "https://example.com/v1/things"
    );
    assert_eq!(super::shell_quote("--header"), "--header");
    assert_eq!(super::shell_quote("a/b_c.txt"), "a/b_c.txt");
}

#[test]
fn shell_quote_wraps_unsafe_args_in_single_quotes() {
    assert_eq!(super::shell_quote(""), "''");
    assert_eq!(super::shell_quote("a b"), "'a b'");
    assert_eq!(super::shell_quote("{\"x\":1}"), "'{\"x\":1}'");
    assert_eq!(super::shell_quote("Bearer abc!@#"), "'Bearer abc!@#'");
}

#[test]
fn shell_quote_escapes_embedded_single_quote() {
    // The standard `'\''` (close, escape, re-open) sequence so the
    // resulting line is paste-back-into-shell safe.
    assert_eq!(super::shell_quote("can't"), "'can'\\''t'");
}

#[test]
fn render_detail_emits_bold_command_then_quoted_args() {
    use vetter_core::{
        Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };
    let p = ParsedCommand {
        command: "curl".into(),
        argv: vec![
            "curl".into(),
            "-X".into(),
            "POST".into(),
            "https://api.example.test/v1/things".into(),
            "-d".into(),
            "{\"x\":1}".into(),
        ],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Post,
            url: url::Url::parse("https://api.example.test/v1/things").unwrap(),
            headers: vec![],
            body: Body::None,
            auth: None,
            tls: TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        })],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    let rendered = super::render_detail(&p);
    assert_eq!(
        rendered,
        "\x1b[1mcurl\x1b[0m -X POST https://api.example.test/v1/things -d '{\"x\":1}'"
    );
}

#[test]
fn render_detail_handles_empty_argv() {
    use vetter_core::{DisplayHints, ParsedCommand};
    let p = ParsedCommand {
        command: "curl".into(),
        argv: vec![],
        cwd: None,
        stdin_digest: None,
        effects: vec![],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    assert_eq!(super::render_detail(&p), "");
}
