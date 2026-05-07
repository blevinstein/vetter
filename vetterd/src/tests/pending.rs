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
        signals: Vec::new(),
        parsed: None,
        host_known: Vec::new(),
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

/// Pins the contract the macOS notifier coalescing relies on: the
/// hint returned by `submit_with_render` is `was_empty_before == true`
/// only for the request that takes the queue out of the empty state.
#[test]
fn submit_marks_first_entry_as_empty_before() {
    let q = PendingQueue::new();

    let (rx_a, hint_a) = q.submit_with_render(summary("a", "https://a.test/"), "rendered-a".into());
    assert!(hint_a.was_empty_before, "first submit must be `first`");

    let (_rx_b, hint_b) =
        q.submit_with_render(summary("b", "https://b.test/"), "rendered-b".into());
    assert!(
        !hint_b.was_empty_before,
        "second submit during a burst must coalesce"
    );

    // Drain both entries so the queue is empty again, then confirm
    // the next submit is a fresh "first".
    assert!(q.resolve("a", PendingDecision::allow("ok-a")));
    let _ = rx_a.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(q.resolve("b", PendingDecision::deny("ok-b")));
    assert!(q.is_empty());

    let (_rx_c, hint_c) =
        q.submit_with_render(summary("c", "https://c.test/"), "rendered-c".into());
    assert!(
        hint_c.was_empty_before,
        "post-drain submit must be `first` again"
    );
}

// ─── Resolved history tests ───────────────────────────────────────────────

#[test]
fn resolve_populates_history() {
    let q = PendingQueue::new();
    let _rx = q.submit_with_render(summary("a", "https://a.test/"), "rendered-a".into());
    assert!(q.resolve("a", PendingDecision::allow("ok")));

    let hist = q.resolved_entries();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].summary.id, "a");
    assert_eq!(hist[0].rendered, "rendered-a");
    assert_eq!(hist[0].decision, WireDecision::Allow);
}

#[test]
fn history_order_newest_first() {
    let q = PendingQueue::new();
    for id in ["first", "second", "third"] {
        let _rx = q.submit(summary(id, "https://example.test/"));
        q.resolve(id, PendingDecision::allow("ok"));
    }
    let hist = q.resolved_entries();
    let ids: Vec<&str> = hist.iter().map(|e| e.summary.id.as_str()).collect();
    assert_eq!(ids, vec!["third", "second", "first"]);
}

#[test]
fn history_cap_respected() {
    let q = PendingQueue::new();
    for i in 0..RESOLVED_CAP + 5 {
        let id = format!("id-{i}");
        let _rx = q.submit(summary(&id, "https://example.test/"));
        q.resolve(&id, PendingDecision::deny("ok"));
    }
    assert_eq!(q.resolved_entries().len(), RESOLVED_CAP);
    // The most-recent entries should be kept; the oldest evicted.
    let newest_id = format!("id-{}", RESOLVED_CAP + 4);
    assert_eq!(q.resolved_entries()[0].summary.id, newest_id);
}

#[test]
fn cancel_all_clears_resolved() {
    let q = PendingQueue::new();
    let _rx = q.submit(summary("a", "https://a.test/"));
    q.resolve("a", PendingDecision::allow("ok"));
    assert_eq!(q.resolved_entries().len(), 1);

    // Submit a second entry so cancel_all has something pending to clear
    // (the change listener fires only when there were pending entries).
    let _rx2 = q.submit(summary("b", "https://b.test/"));
    q.cancel_all();
    assert!(q.resolved_entries().is_empty());
}

#[test]
fn all_entries_returns_consistent_snapshot() {
    let q = PendingQueue::new();
    let _rx_a = q.submit_with_render(summary("a", "https://a.test/"), "rendered-a".into());
    let _rx_b = q.submit_with_render(summary("b", "https://b.test/"), "rendered-b".into());
    q.resolve("a", PendingDecision::deny("rejected"));

    let (pending, resolved) = q.all_entries();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0.id, "b");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].summary.id, "a");
    assert_eq!(resolved[0].decision, WireDecision::Deny);
}

/// Race regression: when N threads call `submit_with_render`
/// concurrently against an initially-empty queue, exactly one
/// observes `was_empty_before == true`. Without the
/// "compute-hint-inside-the-critical-section" property the value
/// is racy and the macOS notifier could either drop the only
/// banner (both threads see "not empty") or emit two (both threads
/// see "empty").
#[test]
fn concurrent_submits_have_exactly_one_first() {
    const THREADS: usize = 32;

    let q = Arc::new(PendingQueue::new());
    let firsts = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(std::sync::Barrier::new(THREADS));

    let mut handles = Vec::new();
    for i in 0..THREADS {
        let q = Arc::clone(&q);
        let firsts = Arc::clone(&firsts);
        let start = Arc::clone(&start);
        handles.push(thread::spawn(move || {
            // Maximise contention: every thread parks at the
            // barrier, then races into the queue's mutex together.
            start.wait();
            let id = format!("id-{i}");
            let (_rx, hint) = q.submit_with_render(summary(&id, "https://x.test/"), String::new());
            if hint.was_empty_before {
                firsts.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        firsts.load(Ordering::SeqCst),
        1,
        "exactly one concurrent submit should observe an empty queue"
    );
    assert_eq!(q.len(), THREADS);
}

// -- warm_resolved ---------------------------------------------

fn resolved(id: &str, decision: WireDecision) -> ResolvedEntry {
    ResolvedEntry {
        summary: summary(id, "https://example.test/"),
        rendered: format!("rendered-{id}"),
        decision,
        rule_id: None,
        rule_scope: None,
    }
}

#[test]
fn warm_resolved_populates_ring_in_order() {
    let q = PendingQueue::new();
    // Caller hands entries newest-first; the ring should surface
    // them in the same order via `resolved_entries()`.
    q.warm_resolved(vec![
        resolved("newest", WireDecision::Allow),
        resolved("middle", WireDecision::Deny),
        resolved("oldest", WireDecision::Allow),
    ]);
    let got = q.resolved_entries();
    let ids: Vec<&str> = got.iter().map(|e| e.summary.id.as_str()).collect();
    assert_eq!(ids, vec!["newest", "middle", "oldest"]);
    assert_eq!(got[0].decision, WireDecision::Allow);
    assert_eq!(got[1].decision, WireDecision::Deny);
}

#[test]
fn warm_resolved_respects_cap() {
    let q = PendingQueue::new();
    let entries: Vec<ResolvedEntry> = (0..RESOLVED_CAP + 5)
        .map(|i| resolved(&format!("id-{i}"), WireDecision::Allow))
        .collect();
    q.warm_resolved(entries);
    assert_eq!(q.resolved_entries().len(), RESOLVED_CAP);
    // We kept the first RESOLVED_CAP entries the caller handed us
    // (newest-first), i.e. `id-0`..`id-{RESOLVED_CAP - 1}`.
    assert_eq!(q.resolved_entries()[0].summary.id, "id-0");
}

#[test]
fn warm_resolved_fires_change_listener_once() {
    let q = Arc::new(PendingQueue::new());
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = Arc::clone(&count);
    q.set_change_listener(move || {
        count2.fetch_add(1, Ordering::SeqCst);
    });
    q.warm_resolved(vec![
        resolved("a", WireDecision::Allow),
        resolved("b", WireDecision::Allow),
    ]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // Empty warm should not fire.
    q.warm_resolved(Vec::<ResolvedEntry>::new());
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn try_from_audit_accepts_rich_prompt_row() {
    use vetter_core::parsers::{
        Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };
    let parsed = ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into(), "https://example.test/".into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: url::Url::parse("https://example.test/").unwrap(),
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
    let audit = crate::audit::AuditEntry {
        id: "abc".into(),
        timestamp: "epoch:0.0".into(),
        command: "curl".into(),
        argv: vec!["curl".into(), "https://example.test/".into()],
        decision: WireDecision::Allow,
        reason: "approved".into(),
        rule_id: None,
        rule_scope: None,
        force_prompt: false,
        primary_verb: "GET".into(),
        primary_target: "https://example.test/".into(),
        signals: Vec::new(),
        parsed: Some(parsed),
        host_known: vec![false],
        rendered: "rendered".into(),
    };
    let entry = ResolvedEntry::try_from_audit(audit).expect("prompt row");
    assert_eq!(entry.summary.id, "abc");
    assert_eq!(entry.summary.primary_verb, "GET");
    assert_eq!(entry.decision, WireDecision::Allow);
    assert_eq!(entry.rendered, "rendered");
    assert!(entry.rule_id.is_none());
    assert!(entry.rule_scope.is_none());
}

#[test]
fn try_from_audit_rejects_no_card_row() {
    let audit = crate::audit::AuditEntry {
        id: "nocard".into(),
        timestamp: "epoch:0.0".into(),
        command: "curl".into(),
        argv: vec!["curl".into(), "https://example.test/".into()],
        decision: WireDecision::Allow,
        reason: "rule allow".into(),
        rule_id: None,
        rule_scope: None,
        force_prompt: false,
        primary_verb: String::new(),
        primary_target: String::new(),
        signals: Vec::new(),
        parsed: None,
        host_known: Vec::new(),
        rendered: String::new(),
    };
    assert!(ResolvedEntry::try_from_audit(audit).is_none());
}

// -- PromptSummary serde round-trip ----------------------------

/// The new `parsed`, `host_known`, and `signals` fields must
/// survive a JSON round-trip; this is the contract the macOS
/// notifier (and any future remote UI) reads from. Older mock
/// notifiers that ignore the new fields keep working because all
/// three carry `#[serde(default)]`; this test pins the *forward*
/// direction (full → full) explicitly.
#[test]
fn prompt_summary_round_trips_through_serde_with_new_fields() {
    use vetter_core::parsers::{
        Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };
    use vetter_core::signals::{RiskSignal, SignalKind};

    let parsed = ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into(), "https://example.test/".into()],
        cwd: Some("/work".into()),
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: url::Url::parse("https://example.test/").unwrap(),
            headers: vec![],
            body: Body::None,
            auth: None,
            tls: TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        })],
        signals: vec![RiskSignal {
            kind: SignalKind::UnknownHost,
            detail: "host example.test not in known-hosts list".into(),
            effect_idx: Some(0),
        }],
        display_hints: DisplayHints {
            primary_verb: "GET".into(),
            primary_target: "https://example.test/".into(),
            badges: vec![],
        },
        extras: serde_json::Value::Null,
    };

    let original = PromptSummary {
        id: "abc".into(),
        command: "curl".into(),
        primary_verb: "GET".into(),
        primary_target: "https://example.test/".into(),
        force_prompt: false,
        signals: parsed.signals.clone(),
        parsed: Some(parsed.clone()),
        host_known: vec![false],
    };

    let json = serde_json::to_string(&original).expect("serialise");
    let restored: PromptSummary = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(restored, original);

    // Belt-and-braces: an older payload missing all three new
    // fields still deserialises (the default empties / None apply).
    let legacy = r#"{
        "id":"x","command":"curl","primary_verb":"GET",
        "primary_target":"https://example.test/","force_prompt":false
    }"#;
    let legacy_decoded: PromptSummary = serde_json::from_str(legacy).expect("legacy decode");
    assert!(legacy_decoded.signals.is_empty());
    assert!(legacy_decoded.parsed.is_none());
    assert!(legacy_decoded.host_known.is_empty());
}

// -- refresh_with: pending + resolved coverage -----------------

/// Build a `PromptSummary` whose `parsed` describes a single GET to
/// `host` and whose `signals` already carries the matching
/// `UnknownHost` record (the state a freshly-submitted unknown-host
/// request would have on the wire).
fn unknown_host_summary(id: &str, host: &str) -> PromptSummary {
    use vetter_core::parsers::{
        Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };
    use vetter_core::signals::{RiskSignal, SignalKind};

    let url = url::Url::parse(&format!("https://{host}/v1/data")).expect("test url");
    let parsed = ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into(), url.as_str().into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: url.clone(),
            headers: vec![],
            body: Body::None,
            auth: None,
            tls: TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        })],
        signals: vec![RiskSignal {
            kind: SignalKind::UnknownHost,
            detail: format!("host {host} not in known-hosts list"),
            effect_idx: Some(0),
        }],
        display_hints: DisplayHints {
            primary_verb: "GET".into(),
            primary_target: url.to_string(),
            badges: vec![],
        },
        extras: serde_json::Value::Null,
    };
    PromptSummary {
        id: id.into(),
        command: "curl".into(),
        primary_verb: "GET".into(),
        primary_target: url.to_string(),
        force_prompt: false,
        signals: parsed.signals.clone(),
        parsed: Some(parsed),
        host_known: vec![false],
    }
}

/// Build a known-hosts store whose user layer trusts `pattern`. We
/// skip the disk loader and construct the store directly so the test
/// stays hermetic (no `$HOME` writes, no tempdir cleanup).
fn known_hosts_with(pattern: &str) -> vetter_core::known_hosts::KnownHostsStore {
    use vetter_core::known_hosts::{KnownHostEntry, KnownHostsStore};
    KnownHostsStore {
        builtin: vec![],
        user: vec![KnownHostEntry {
            pattern: pattern.into(),
            note: None,
        }],
        project: vec![],
    }
}

#[test]
fn refresh_with_clears_unknown_host_signal_on_resolved_entry() {
    use vetter_core::SignalKind;

    let q = PendingQueue::new();
    let _rx = q.submit_with_render(
        unknown_host_summary("a", "api.unknown.test"),
        "rendered-a".into(),
    );
    assert!(q.resolve("a", PendingDecision::allow("approved")));

    // Sanity: the resolved entry currently carries the stale
    // UnknownHost signal and host_known == [false].
    let before = q.resolved_entries();
    assert_eq!(before.len(), 1);
    assert!(before[0]
        .summary
        .signals
        .iter()
        .any(|s| s.kind == SignalKind::UnknownHost));
    assert_eq!(before[0].summary.host_known, vec![false]);

    q.refresh_with(&known_hosts_with("api.unknown.test"));

    let after = q.resolved_entries();
    assert_eq!(after.len(), 1);
    assert!(
        !after[0]
            .summary
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::UnknownHost),
        "UnknownHost signal should be gone after refresh: {:?}",
        after[0].summary.signals
    );
    assert_eq!(
        after[0].summary.host_known,
        vec![true],
        "host_known should flip to true for the now-trusted host"
    );
}

#[test]
fn refresh_with_fires_listener_when_only_resolved_entries_present() {
    let q = Arc::new(PendingQueue::new());
    let _rx = q.submit_with_render(
        unknown_host_summary("a", "api.unknown.test"),
        "rendered-a".into(),
    );
    assert!(q.resolve("a", PendingDecision::allow("approved")));
    assert!(q.is_empty(), "pending should be empty after resolve");

    // Install the listener *after* the resolve so the submit/resolve
    // ticks above don't pollute the count.
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = Arc::clone(&count);
    q.set_change_listener(move || {
        count2.fetch_add(1, Ordering::SeqCst);
    });

    q.refresh_with(&known_hosts_with("api.unknown.test"));
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "listener should fire exactly once for a refresh that only touches resolved entries"
    );

    // Belt-and-braces: a refresh on a fully-empty queue stays quiet.
    let q_empty = Arc::new(PendingQueue::new());
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = Arc::clone(&count);
    q_empty.set_change_listener(move || {
        count2.fetch_add(1, Ordering::SeqCst);
    });
    q_empty.refresh_with(&known_hosts_with("api.unknown.test"));
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "empty queue refresh must not redraw"
    );
}

// -- record_auto -----------------------------------------------

/// Auto-decisions skip the pending map entirely and land directly on
/// the resolved-history ring with their matcher attribution intact.
/// The popover's "See approval reason" disclosure reads back the
/// `(rule_id, rule_scope)` pair stamped here.
#[test]
fn record_auto_pushes_entry_with_attribution_onto_ring() {
    use vetter_core::matcher::Scope;
    let q = PendingQueue::new();
    q.record_auto(
        summary("auto-1", "https://api.test/"),
        "rendered-auto".into(),
        WireDecision::Allow,
        Some("trust-api".into()),
        Some(Scope::User),
    );
    let ring = q.resolved_entries();
    assert_eq!(ring.len(), 1);
    assert_eq!(ring[0].summary.id, "auto-1");
    assert_eq!(ring[0].decision, WireDecision::Allow);
    assert_eq!(ring[0].rendered, "rendered-auto");
    assert_eq!(ring[0].rule_id.as_deref(), Some("trust-api"));
    assert_eq!(ring[0].rule_scope, Some(Scope::User));
    assert!(q.is_empty(), "auto path must not park anything as pending");
}

/// Auto-decisions evict the oldest ring entry once the cap is hit,
/// just like the human-resolved path. Without this the popover's
/// Recent section would grow unbounded as auto-allow traffic
/// dominates.
#[test]
fn record_auto_evicts_oldest_when_ring_full() {
    use vetter_core::matcher::Scope;
    let q = PendingQueue::new();
    for i in 0..RESOLVED_CAP {
        q.record_auto(
            summary(&format!("auto-{i}"), "https://api.test/"),
            "rendered".into(),
            WireDecision::Allow,
            Some(format!("rule-{i}")),
            Some(Scope::User),
        );
    }
    assert_eq!(q.resolved_entries().len(), RESOLVED_CAP);

    q.record_auto(
        summary("overflow", "https://api.test/"),
        "rendered".into(),
        WireDecision::Allow,
        Some("rule-overflow".into()),
        Some(Scope::User),
    );
    let ring = q.resolved_entries();
    assert_eq!(ring.len(), RESOLVED_CAP);
    assert_eq!(ring[0].summary.id, "overflow");
    // The oldest entry (`auto-0`) should have rolled off the back.
    assert!(ring.iter().all(|e| e.summary.id != "auto-0"));
}

#[test]
fn record_auto_fires_change_listener() {
    use vetter_core::matcher::Scope;
    let q = Arc::new(PendingQueue::new());
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = Arc::clone(&count);
    q.set_change_listener(move || {
        count2.fetch_add(1, Ordering::SeqCst);
    });
    q.record_auto(
        summary("auto-1", "https://api.test/"),
        "rendered".into(),
        WireDecision::Allow,
        Some("trust-api".into()),
        Some(Scope::User),
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
