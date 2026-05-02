//! Tests for [`crate::pending`]. Layout convention is described in
//! `AGENTS.md`.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::*;
use vetter_core::wire::WireDecision;

fn summary(id: &str, target: &str) -> PromptSummary {
    PromptSummary {
        id: id.into(),
        command: "curl".into(),
        primary_verb: "GET".into(),
        primary_target: target.into(),
        force_prompt: false,
    }
}

#[test]
fn submit_then_resolve_wakes_receiver() {
    let q = PendingQueue::new();
    let rx = q.submit(summary("abc", "https://example.test/"));
    assert_eq!(q.len(), 1);
    assert!(q.resolve("abc", PendingDecision::allow("user clicked Approve")));
    let got = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("receiver should wake");
    assert_eq!(got.decision, WireDecision::Allow);
    assert_eq!(got.reason, "user clicked Approve");
    assert_eq!(q.len(), 0);
}

#[test]
fn resolve_unknown_id_is_a_no_op() {
    let q = PendingQueue::new();
    assert!(!q.resolve("nope", PendingDecision::allow("ghost")));
}

#[test]
fn resolve_twice_only_wakes_first_waiter() {
    let q = PendingQueue::new();
    let _rx = q.submit(summary("abc", "https://example.test/"));
    assert!(q.resolve("abc", PendingDecision::allow("first")));
    // Second resolve targets a now-unknown id.
    assert!(!q.resolve("abc", PendingDecision::deny("second")));
}

#[test]
fn two_concurrent_submits_get_independent_decisions() {
    let q = Arc::new(PendingQueue::new());
    let rx_a = q.submit(summary("a", "https://a.test/"));
    let rx_b = q.submit(summary("b", "https://b.test/"));
    assert_eq!(q.len(), 2);

    let q2 = Arc::clone(&q);
    let resolver = thread::spawn(move || {
        // Resolve in reverse order to confirm there's no head-of-line
        // dependency between waiters.
        thread::sleep(Duration::from_millis(20));
        assert!(q2.resolve("b", PendingDecision::deny("Reject b")));
        thread::sleep(Duration::from_millis(20));
        assert!(q2.resolve("a", PendingDecision::allow("Allow a")));
    });

    let dec_b = rx_b.recv_timeout(Duration::from_secs(1)).unwrap();
    let dec_a = rx_a.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(dec_b.decision, WireDecision::Deny);
    assert_eq!(dec_a.decision, WireDecision::Allow);
    resolver.join().unwrap();
    assert_eq!(q.len(), 0);
}

#[test]
fn cancel_all_disconnects_outstanding_receivers() {
    let q = PendingQueue::new();
    let rx_a = q.submit(summary("a", "https://a.test/"));
    let rx_b = q.submit(summary("b", "https://b.test/"));
    q.cancel_all();
    assert_eq!(q.len(), 0);
    assert!(rx_a.recv_timeout(Duration::from_secs(1)).is_err());
    assert!(rx_b.recv_timeout(Duration::from_secs(1)).is_err());
}

#[test]
fn pending_summaries_lists_outstanding_entries() {
    let q = PendingQueue::new();
    let _rx = q.submit(summary("a", "https://a.test/"));
    let _rx = q.submit(summary("b", "https://b.test/"));
    let mut targets: Vec<String> = q
        .pending_summaries()
        .into_iter()
        .map(|s| s.primary_target)
        .collect();
    targets.sort();
    assert_eq!(targets, vec!["https://a.test/", "https://b.test/"]);
}

#[test]
fn shutdown_reason_is_a_human_string() {
    // Lock the constant down: tests across the workspace assert on it.
    assert!(SHUTDOWN_REASON.contains("shut"));
}
