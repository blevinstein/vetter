//! Tests for [`crate::pending`]. Layout convention is described in
//! `AGENTS.md`.

use std::sync::atomic::{AtomicUsize, Ordering};
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
fn pending_entries_carries_rendered_detail() {
    let q = PendingQueue::new();
    let _rx_a = q.submit_with_render(summary("a", "https://a.test/"), "rendered-a".into());
    let _rx_b = q.submit(summary("b", "https://b.test/"));
    let mut got: Vec<(String, String)> = q
        .pending_entries()
        .into_iter()
        .map(|(s, r)| (s.id, r))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("a".into(), "rendered-a".into()),
            ("b".into(), String::new()),
        ]
    );
}

#[test]
fn change_listener_fires_on_submit_and_resolve() {
    let q = Arc::new(PendingQueue::new());
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = Arc::clone(&count);
    q.set_change_listener(move || {
        count2.fetch_add(1, Ordering::SeqCst);
    });

    let _rx = q.submit(summary("a", "https://a.test/"));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let _rx = q.submit(summary("b", "https://b.test/"));
    assert_eq!(count.load(Ordering::SeqCst), 2);

    assert!(q.resolve("a", PendingDecision::allow("ok")));
    assert_eq!(count.load(Ordering::SeqCst), 3);

    // Resolving an unknown id must NOT fire the listener.
    assert!(!q.resolve("ghost", PendingDecision::deny("nope")));
    assert_eq!(count.load(Ordering::SeqCst), 3);

    q.cancel_all();
    assert_eq!(count.load(Ordering::SeqCst), 4);

    // cancel_all on an empty queue is a no-op for the listener.
    q.cancel_all();
    assert_eq!(count.load(Ordering::SeqCst), 4);
}

#[test]
fn change_listener_does_not_deadlock_under_contention() {
    // Listener that re-enters the queue's read-only API: a real
    // implementation (the popover refresh path) does exactly this
    // — it calls `pending_entries` from inside the callback. The
    // queue must have released its lock before invoking the listener.
    let q = Arc::new(PendingQueue::new());
    let q_for_cb = Arc::clone(&q);
    q.set_change_listener(move || {
        let _entries = q_for_cb.pending_entries();
    });

    let mut handles = Vec::new();
    for i in 0..16 {
        let q = Arc::clone(&q);
        handles.push(thread::spawn(move || {
            let id = format!("id-{i}");
            let _rx = q.submit(summary(&id, "https://a.test/"));
            // small jitter to interleave threads
            thread::sleep(Duration::from_millis(2));
            q.resolve(&id, PendingDecision::allow("ok"));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(q.len(), 0);
}

#[test]
fn shutdown_reason_is_a_human_string() {
    // Lock the constant down: tests across the workspace assert on it.
    assert!(SHUTDOWN_REASON.contains("shut"));
}
