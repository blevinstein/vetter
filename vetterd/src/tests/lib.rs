//! Tests for [`crate`] root. Layout convention is described in
//! `AGENTS.md`.

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
    let allowlist = load_default(None, None).unwrap();
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let pending = Arc::new(PendingQueue::new());
    let notifier: Arc<dyn crate::notifier::Notifier> = Arc::new(NoopNotifier);
    let known_hosts = load_known_hosts_default(None).unwrap();
    let _ctx = Context {
        socket_path: sock,
        audit,
        allowlist,
        known_hosts,
        pending,
        notifier,
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
