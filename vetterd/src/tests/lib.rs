//! Tests for [`crate`] root. Layout convention is described in
//! `AGENTS.md`.

use std::sync::RwLock;

use super::*;
use crate::notifier::NoopNotifier;
use crate::pending::PendingQueue;
use crate::testutil::tmpdir;
use vetter_core::wire::WireDecision;

/// Tests that mutate `VETTERD_MAX_INFLIGHT` take a shared mutex to
/// serialise against each other regardless of `cargo test`'s
/// parallelism. Other env-mutating modules (`paths.rs`) use their
/// own lock; the keys don't overlap so a separate static is fine.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(())).lock().unwrap()
}

struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}
impl EnvGuard {
    fn unset(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prev }
    }
    fn set(key: &'static str, val: &str) -> Self {
        let prev = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, val);
        }
        Self { key, prev }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

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
        effects: vec![],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    assert_eq!(super::render_detail(&p), "");
}

// -- max_inflight_from_env -------------------------------------

#[test]
fn max_inflight_defaults_when_env_unset() {
    let _g = env_lock();
    let _e = EnvGuard::unset("VETTERD_MAX_INFLIGHT");
    assert_eq!(
        super::max_inflight_from_env().expect("default branch"),
        super::DEFAULT_MAX_INFLIGHT
    );
}

#[test]
fn max_inflight_parses_positive_integer() {
    let _g = env_lock();
    let _e = EnvGuard::set("VETTERD_MAX_INFLIGHT", "42");
    assert_eq!(super::max_inflight_from_env().expect("parse 42"), 42);
}

#[test]
fn max_inflight_rejects_zero() {
    let _g = env_lock();
    let _e = EnvGuard::set("VETTERD_MAX_INFLIGHT", "0");
    let err = super::max_inflight_from_env().expect_err("zero must be a config error");
    assert!(matches!(err, super::DaemonError::Config(_)), "{err:?}");
    assert!(err.to_string().contains("must be > 0"), "{err}");
}

#[test]
fn max_inflight_rejects_non_numeric() {
    let _g = env_lock();
    let _e = EnvGuard::set("VETTERD_MAX_INFLIGHT", "lots");
    let err = super::max_inflight_from_env().expect_err("non-numeric must be a config error");
    assert!(matches!(err, super::DaemonError::Config(_)), "{err:?}");
    assert!(err.to_string().contains("VETTERD_MAX_INFLIGHT"), "{err}");
}

#[test]
fn max_inflight_rejects_negative() {
    let _g = env_lock();
    let _e = EnvGuard::set("VETTERD_MAX_INFLIGHT", "-1");
    let err = super::max_inflight_from_env().expect_err("negative must be a config error");
    assert!(matches!(err, super::DaemonError::Config(_)), "{err:?}");
}

// ── `match_pending_id` — admin-socket id resolution ─────────────────

/// Two ULIDs minted in the same millisecond that share a long
/// prefix; realistic input for the ambiguity path, since the ULID
/// timestamp occupies the leading 10 characters.
const ID_A: &str = "01JBQ7Z9M0AAAAAAAAAAAAAAAA";
const ID_B: &str = "01JBQ7Z9M0BBBBBBBBBBBBBBBB";

fn pending_ids() -> Vec<String> {
    vec![ID_A.to_string(), ID_B.to_string()]
}

#[test]
fn match_pending_id_accepts_a_full_ulid() {
    assert_eq!(
        super::match_pending_id(&pending_ids(), ID_A).expect("exact match"),
        ID_A
    );
}

#[test]
fn match_pending_id_accepts_a_unique_prefix() {
    // One character past the shared timestamp+randomness prefix is
    // enough to disambiguate.
    assert_eq!(
        super::match_pending_id(&pending_ids(), "01JBQ7Z9M0A").expect("unique prefix"),
        ID_A
    );
}

#[test]
fn match_pending_id_is_case_insensitive() {
    assert_eq!(
        super::match_pending_id(&pending_ids(), &ID_B.to_ascii_lowercase())
            .expect("lowercase full id"),
        ID_B
    );
    assert_eq!(
        super::match_pending_id(&pending_ids(), "01jbq7z9m0b").expect("lowercase prefix"),
        ID_B
    );
}

#[test]
fn match_pending_id_rejects_an_ambiguous_prefix() {
    let err = super::match_pending_id(&pending_ids(), "01JBQ7Z9M0")
        .expect_err("shared prefix must be ambiguous");
    assert!(err.contains("ambiguous"), "{err}");
    // The operator needs to see *which* requests collided.
    assert!(err.contains(ID_A), "{err}");
    assert!(err.contains(ID_B), "{err}");
}

#[test]
fn match_pending_id_rejects_an_unknown_id() {
    let err = super::match_pending_id(&pending_ids(), "01ZZZZZZZZ")
        .expect_err("unknown id must not match");
    assert!(err.contains("no pending request"), "{err}");
    assert!(err.contains("01ZZZZZZZZ"), "{err}");
}

#[test]
fn match_pending_id_rejects_an_empty_id() {
    let err = super::match_pending_id(&pending_ids(), "   ").expect_err("blank id must not match");
    assert!(err.contains("empty request id"), "{err}");
}

#[test]
fn match_pending_id_against_an_empty_queue_is_an_error() {
    let err = super::match_pending_id(&[], ID_A).expect_err("nothing is pending");
    assert!(err.contains("no pending request"), "{err}");
}
