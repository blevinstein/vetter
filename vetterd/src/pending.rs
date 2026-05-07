//! Pending-prompt request queue.
//!
//! Mediates between connection workers and the approval UI: a worker
//! that hits a prompt-class outcome calls [`PendingQueue::submit`] (or
//! [`PendingQueue::submit_with_render`] when a §8.5 detail string is
//! available), gets a [`Receiver`], and blocks on it. The UI delegate
//! (real [`crate::notifier::Notifier`] or
//! [`crate::notifier::MockNotifier`]) calls [`PendingQueue::resolve`]
//! from whatever thread the platform gives it; the worker wakes with a
//! [`PendingDecision`] and writes the wire reply.
//!
//! `PendingQueue` is owned by the daemon's [`crate::Context`] and
//! shared across all worker threads through an `Arc`. Internally it's
//! a `Mutex<HashMap>`; the critical section is bounded by `insert`,
//! `remove`, or a `clone` of stored summaries — sub-millisecond at
//! worst.
//!
//! Shutdown: [`PendingQueue::cancel_all`] drops every sender, which
//! wakes blocked receivers with `Err(RecvError)` so the connection
//! worker can fall back to an explicit deny instead of hanging.
//!
//! Change listener: [`PendingQueue::set_change_listener`] registers a
//! single observer that fires after every state-changing operation
//! (`submit`, `submit_with_render`, `resolve` that hit an entry, and
//! `cancel_all` when entries were dropped). The macOS popover wires
//! this through `DispatchQueue::main` to refresh its card list and the
//! menu-bar icon's pending-count badge. The queue itself stays
//! platform-agnostic — listeners are responsible for hopping to the
//! correct thread.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use vetter_core::matcher::Scope;
use vetter_core::wire::WireDecision;

/// Compact one-line UI summary of a prompt-class request. Built by
/// the daemon from the re-parsed [`vetter_core::ParsedCommand`] and
/// passed verbatim into the notifier so every UI surface (real
/// notification, mock control socket, future popover detail view)
/// renders the same fields.
// `Eq` was dropped when `parsed: Option<ParsedCommand>` was added —
// `ParsedCommand` carries `serde_json::Value` (no `Eq` impl) and
// floats inside parser extras, so we only get `PartialEq`. Tests
// that need equality use the partial form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptSummary {
    /// Correlation id, equal to [`vetter_core::wire::VetRequest::id`].
    pub id: String,
    /// Stable parser identifier, e.g. `"curl"`.
    pub command: String,
    /// Display verb from [`vetter_core::DisplayHints::primary_verb`]
    /// (e.g. `"GET"`, `"POST"`). Empty when the parser didn't set one.
    pub primary_verb: String,
    /// Display target from [`vetter_core::DisplayHints::primary_target`]
    /// (typically a normalised URL).
    pub primary_target: String,
    /// True when the user passed `--dry-run` (i.e. wants the prompt
    /// path even if a permissive rule would auto-allow). Surfaced so
    /// the UI can label dry-run prompts distinctly.
    pub force_prompt: bool,
    /// Full risk-signal records attached to the parsed command —
    /// kind, human detail, and (optional) effect index. Surfaced
    /// here so the macOS approver popover can paint `Danger`/`Warn`
    /// signal pills (kind → severity → colour) and feed the
    /// per-pill tooltip text from `RiskSignal::detail` without
    /// dragging the analyzer back into the UI process.
    ///
    /// `#[serde(default)]` keeps the mock-socket protocol forward-
    /// compatible: older mock harnesses (pre-native-UI) ship
    /// summaries without this field, deserialise to an empty list,
    /// and skip the pill row entirely. (Earlier versions of this
    /// field carried `Vec<SignalKind>`; serde tolerates that schema
    /// drift because the new wire form is a strict superset and the
    /// in-tree mocks ignore unknown fields.)
    #[serde(default)]
    pub signals: Vec<vetter_core::RiskSignal>,
    /// Full parsed command. `Some` on every prompt-class request
    /// produced by the daemon (Phase 4 onward); `None` for legacy /
    /// mock callers that pre-date this field, in which case the
    /// popover degrades to "URL row from `primary_target` plus the
    /// 'Show raw' disclosure". The native UI uses this to walk
    /// `parsed.effects` and produce per-effect rows (headers,
    /// body, auth, file ops, process spawns) without re-running
    /// the parser on the UI side.
    #[serde(default)]
    pub parsed: Option<vetter_core::ParsedCommand>,
    /// Per-effect host-trust hints, indexed by `parsed.effects`
    /// position. Entry `i` is `true` iff `effects[i]` is an
    /// `HttpRequest` whose host matched the daemon's
    /// `KnownHostsStore` (loopback hosts also count as known —
    /// they're "trusted local"). Non-HttpRequest effects get
    /// `false`; the URL row falls back to a plain
    /// `<verb> <target>` label for those slots.
    ///
    /// Computed daemon-side because the `KnownHostsStore` lives
    /// behind the IPC boundary; the popover would otherwise need
    /// to load the same YAML files itself.
    #[serde(default)]
    pub host_known: Vec<bool>,
}

/// A resolved request kept in the recent-history ring. Carries the
/// original summary and rendered detail (so the popover can display
/// the same card body as when the request was pending) plus the
/// decision that was made. After Phase 5.1 auto-allowed entries also
/// land here, attributed via [`Self::rule_id`] / [`Self::rule_scope`]
/// so the popover's "See approval reason" disclosure can name the
/// rule and offer a Revoke button.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    pub summary: PromptSummary,
    /// Pre-rendered §8.5 detail (same string shown while pending).
    pub rendered: String,
    pub decision: WireDecision,
    /// Allowlist rule id that auto-resolved this request, when the
    /// matcher attributed the decision to a specific rule. `None`
    /// for human-resolved prompt entries (no rule was involved) and
    /// for parse-failure / shutdown decisions.
    pub rule_id: Option<String>,
    /// Allowlist layer that produced [`Self::rule_id`]. Always
    /// populated together with `rule_id`.
    pub rule_scope: Option<Scope>,
}

impl ResolvedEntry {
    /// Reconstruct a ring-entry from an [`crate::audit::AuditEntry`]
    /// tailed off disk at daemon startup. Returns `None` when the
    /// entry lacks the rich popover-card payload (`parsed` / non-
    /// empty `rendered`) — parse-failure rows and other no-card
    /// audit lines don't belong in the popover's "Recent" section.
    ///
    /// This is the only bridge from the durable audit log back into
    /// the in-memory ring; tests lean on it heavily.
    pub fn try_from_audit(entry: crate::audit::AuditEntry) -> Option<Self> {
        if entry.rendered.is_empty() || entry.parsed.is_none() {
            return None;
        }
        let summary = PromptSummary {
            id: entry.id,
            command: entry.command,
            primary_verb: entry.primary_verb,
            primary_target: entry.primary_target,
            force_prompt: entry.force_prompt,
            signals: entry.signals,
            parsed: entry.parsed,
            host_known: entry.host_known,
        };
        Some(Self {
            summary,
            rendered: entry.rendered,
            decision: entry.decision,
            rule_id: entry.rule_id,
            rule_scope: entry.rule_scope,
        })
    }
}

/// Decision posted back by the UI. Mirrors the wire's
/// [`WireDecision`] but carries an explicit human-readable reason so
/// `vet`'s `allow (…)` / `deny (…)` line is informative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDecision {
    pub decision: WireDecision,
    pub reason: String,
}

impl PendingDecision {
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

/// Out-of-band advice from the queue to the notifier, computed
/// inside [`PendingQueue::submit_with_render`]'s critical section
/// so concurrent submits get race-free answers.
///
/// Currently carries a single bit, [`Self::was_empty_before`], that
/// the macOS notifier uses to coalesce: only the request that
/// transitions the queue from empty to non-empty raises a banner.
/// Subsequent requests in the same burst rely on the menu-bar
/// badge and popover (both auto-refresh from the queue's change
/// listener) instead of stacking notifications.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotifyHint {
    /// True iff the queue was empty immediately before this entry
    /// was inserted. Captured in the same critical section as the
    /// insert so two threads racing into [`PendingQueue::submit`]
    /// can never both see "empty".
    pub was_empty_before: bool,
}

struct Entry {
    summary: PromptSummary,
    /// Pre-rendered §8.5 detail string. Empty for callers that go
    /// through the legacy [`PendingQueue::submit`] path (mock /
    /// older tests); the popover degrades gracefully when empty.
    rendered: String,
    sender: Sender<PendingDecision>,
}

/// Single observer fired after every state-changing operation. Stored
/// behind the same mutex as the entries so registration / clearing is
/// race-free, but invoked **after** the lock is dropped to avoid
/// re-entrant deadlocks (a listener is typically `DispatchQueue::main`
/// which itself synchronises against AppKit).
type ChangeListener = Arc<dyn Fn() + Send + Sync>;

/// Thread-safe map of `id → (summary, rendered, sender)`. Owned by
/// the daemon [`crate::Context`].
#[derive(Default)]
pub struct PendingQueue {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    /// Ring of recently-resolved requests, newest at index 0.
    /// Capped at [`RESOLVED_CAP`]; oldest entry is evicted when full.
    resolved: VecDeque<ResolvedEntry>,
    listener: Option<ChangeListener>,
}

/// Maximum number of resolved entries retained in memory. Oldest
/// entries (furthest from the head of the deque) are evicted once
/// this cap is reached. Sized to give a useful recent-history view
/// without unbounded memory growth.
pub const RESOLVED_CAP: usize = 20;

/// Reason filed in the audit log when the daemon shuts down with a
/// connection still parked on `submit`. Single source of truth so
/// tests can grep on it.
pub const SHUTDOWN_REASON: &str = "daemon shutting down";

impl PendingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new prompt-class request with no rendered detail.
    /// Equivalent to [`Self::submit_with_render`] with an empty
    /// string — useful for tests and the mock notifier where the
    /// popover never reads the rendered field.
    ///
    /// Discards the [`NotifyHint`]; callers that need it (the
    /// daemon worker site) call `submit_with_render` directly.
    pub fn submit(&self, summary: PromptSummary) -> Receiver<PendingDecision> {
        self.submit_with_render(summary, String::new()).0
    }

    /// Register a new prompt-class request with its pre-rendered §8.5
    /// detail. Returns the [`Receiver`] that blocks until the
    /// matching id is resolved (or the queue is cancelled — see
    /// [`Self::cancel_all`]) plus a [`NotifyHint`] computed inside
    /// the same critical section as the insert.
    /// The caller should treat `Err(RecvError)` on the receiver as a
    /// deny.
    ///
    /// Duplicate ids overwrite the prior entry — by construction
    /// every request carries a fresh ULID, so this is a defensive
    /// fallback rather than a normal path. When that happens
    /// `was_empty_before` reflects the pre-insert queue state, which
    /// is `false` because the prior entry is still in the map.
    pub fn submit_with_render(
        &self,
        summary: PromptSummary,
        rendered: String,
    ) -> (Receiver<PendingDecision>, NotifyHint) {
        let (tx, rx) = channel();
        let (hint, listener) = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            let hint = NotifyHint {
                was_empty_before: g.entries.is_empty(),
            };
            g.entries.insert(
                summary.id.clone(),
                Entry {
                    summary,
                    rendered,
                    sender: tx,
                },
            );
            (hint, g.listener.clone())
        };
        if let Some(cb) = listener {
            cb();
        }
        (rx, hint)
    }

    /// Post a decision for `id`. Returns `true` if a waiter was
    /// notified, `false` if the id was unknown (e.g. already resolved
    /// or never submitted). Callers can use the return value to log
    /// stale clicks. The change listener fires only when an entry
    /// was actually removed; spurious clicks don't redraw the UI.
    ///
    /// On a successful resolve the entry is pushed to the front of
    /// the [`RESOLVED_CAP`]-sized history ring so the popover can
    /// display recent decisions even when the pending queue is empty.
    pub fn resolve(&self, id: &str, decision: PendingDecision) -> bool {
        let (entry, listener) = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            let removed = g.entries.remove(id);
            if let Some(ref e) = removed {
                g.resolved.push_front(ResolvedEntry {
                    summary: e.summary.clone(),
                    rendered: e.rendered.clone(),
                    decision: decision.decision,
                    rule_id: None,
                    rule_scope: None,
                });
                if g.resolved.len() > RESOLVED_CAP {
                    g.resolved.pop_back();
                }
            }
            (removed, g.listener.clone())
        };
        match entry {
            Some(e) => {
                let sent = e.sender.send(decision).is_ok();
                if let Some(cb) = listener {
                    cb();
                }
                sent
            }
            None => false,
        }
    }

    /// Push an auto-decision (one the matcher resolved without a
    /// human prompt) directly onto the resolved-history ring. The
    /// request was never in the pending map; this method skips the
    /// `entries.remove(...)` step that [`Self::resolve`] performs.
    ///
    /// `rule_id` / `rule_scope` carry the matcher's attribution and
    /// drive the popover's "See approval reason" disclosure + Revoke
    /// button. Both are `Some` for matcher-driven auto decisions and
    /// `None` only on edge paths (e.g. parse failures upstream).
    ///
    /// Fires the change listener so the popover repaints.
    pub fn record_auto(
        &self,
        summary: PromptSummary,
        rendered: String,
        decision: WireDecision,
        rule_id: Option<String>,
        rule_scope: Option<Scope>,
    ) {
        let listener = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            g.resolved.push_front(ResolvedEntry {
                summary,
                rendered,
                decision,
                rule_id,
                rule_scope,
            });
            if g.resolved.len() > RESOLVED_CAP {
                g.resolved.pop_back();
            }
            g.listener.clone()
        };
        if let Some(cb) = listener {
            cb();
        }
    }

    /// Preload the resolved-history ring with entries tailed off disk
    /// at daemon startup. `entries` is expected **newest-first** (the
    /// order [`crate::audit::AuditLog::tail_resolved_entries`] returns)
    /// so we can push straight onto the deque without reversing.
    ///
    /// The ring is capped at [`RESOLVED_CAP`]; we truncate silently
    /// if the caller passes more than that. Fires the change listener
    /// once if the call actually populated any entries, so a popover
    /// whose listener was registered pre-warm also repaints.
    pub fn warm_resolved<I>(&self, entries: I)
    where
        I: IntoIterator<Item = ResolvedEntry>,
    {
        let (added, listener) = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            let mut added = 0usize;
            for entry in entries {
                if g.resolved.len() >= RESOLVED_CAP {
                    break;
                }
                g.resolved.push_back(entry);
                added += 1;
            }
            (added, g.listener.clone())
        };
        if added > 0 {
            if let Some(cb) = listener {
                cb();
            }
        }
    }

    /// Drop every outstanding entry and clear the resolved history.
    /// Receivers wake with `Err(RecvError)`. Used on daemon shutdown
    /// so blocked workers don't hold connections open past the SIGTERM.
    /// Fires the change listener iff there were pending entries.
    pub fn cancel_all(&self) {
        let (had_entries, listener) = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            let had = !g.entries.is_empty();
            g.entries.clear();
            g.resolved.clear();
            (had, g.listener.clone())
        };
        if had_entries {
            if let Some(cb) = listener {
                cb();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("pending mutex poisoned")
            .entries
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot of currently-pending summaries. Cheap clone of the
    /// stored values; the menu-bar popover reads through this via
    /// [`Self::pending_entries`].
    pub fn pending_summaries(&self) -> Vec<PromptSummary> {
        self.inner
            .lock()
            .expect("pending mutex poisoned")
            .entries
            .values()
            .map(|e| e.summary.clone())
            .collect()
    }

    /// Snapshot of currently-pending entries with their rendered
    /// §8.5 detail string. Cheap full clone (a few hundred bytes
    /// each); the popover rebuilds its card list from this on every
    /// `popoverWillShow:` and on every change-listener tick.
    pub fn pending_entries(&self) -> Vec<(PromptSummary, String)> {
        self.inner
            .lock()
            .expect("pending mutex poisoned")
            .entries
            .values()
            .map(|e| (e.summary.clone(), e.rendered.clone()))
            .collect()
    }

    /// Snapshot of recently-resolved entries, newest first. At most
    /// [`RESOLVED_CAP`] entries are retained; the oldest are evicted
    /// automatically on each new resolve.
    pub fn resolved_entries(&self) -> Vec<ResolvedEntry> {
        self.inner
            .lock()
            .expect("pending mutex poisoned")
            .resolved
            .iter()
            .cloned()
            .collect()
    }

    /// Atomically snapshot both pending and resolved entries under a
    /// single lock acquisition. Preferred over calling
    /// [`Self::pending_entries`] and [`Self::resolved_entries`]
    /// separately so the popover always gets a consistent view.
    pub fn all_entries(&self) -> (Vec<(PromptSummary, String)>, Vec<ResolvedEntry>) {
        let g = self.inner.lock().expect("pending mutex poisoned");
        let pending = g
            .entries
            .values()
            .map(|e| (e.summary.clone(), e.rendered.clone()))
            .collect();
        let resolved = g.resolved.iter().cloned().collect();
        (pending, resolved)
    }

    /// Re-derive [`PromptSummary::signals`] and
    /// [`PromptSummary::host_known`] for every pending **and**
    /// recently-resolved entry against `known_hosts`. Used by the
    /// Phase-5 `AddKnownHost` admin handler so the popover repaints
    /// after a host transitions from unknown → known: the host pill
    /// flips green and the `UnknownHost` signal pill disappears
    /// without the user having to re-issue the request. The Recent
    /// section is refreshed alongside the pending section so cards
    /// the user just approved (and is staring at) drop their stale
    /// orange pill at the same moment as still-pending cards.
    ///
    /// Generic risk signals from [`vetter_core::analyze`] are kept
    /// (they don't depend on the known-hosts store) and only the
    /// `UnknownHost` signal is recomputed; we splice the new
    /// `check_known_hosts(...)` output in alongside the existing
    /// non-known-hosts signals so curl-specific signals
    /// (`InsecureFlag`, `ResolveOverride`, ...) survive the refresh.
    ///
    /// Fires the change listener once at the end iff either ring had
    /// at least one entry — a fully-empty queue stays quiet so
    /// listener-side popover redraws don't run on no-op refreshes.
    pub fn refresh_with(&self, known_hosts: &vetter_core::known_hosts::KnownHostsStore) {
        let listener = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            if g.entries.is_empty() && g.resolved.is_empty() {
                return;
            }
            for entry in g.entries.values_mut() {
                refresh_summary(&mut entry.summary, known_hosts);
            }
            for resolved in g.resolved.iter_mut() {
                refresh_summary(&mut resolved.summary, known_hosts);
            }
            g.listener.clone()
        };
        if let Some(cb) = listener {
            cb();
        }
    }

    /// Install a single observer fired after every state change.
    /// Replaces any previously-installed listener. Clear with
    /// [`Self::clear_change_listener`].
    pub fn set_change_listener<F>(&self, cb: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.inner.lock().expect("pending mutex poisoned").listener = Some(Arc::new(cb));
    }

    /// Drop the change listener. Mainly useful in tests that share a
    /// queue across iterations.
    pub fn clear_change_listener(&self) {
        self.inner.lock().expect("pending mutex poisoned").listener = None;
    }
}

/// Re-derive the known-hosts-dependent fields of a single
/// [`PromptSummary`] in place against `known_hosts`. Shared between
/// the pending and resolved walks of [`PendingQueue::refresh_with`]
/// so the two sections always agree on what counts as known.
///
/// No-op when `summary.parsed` is `None` (legacy / mock summaries
/// that pre-date the `parsed` field).
fn refresh_summary(
    summary: &mut PromptSummary,
    known_hosts: &vetter_core::known_hosts::KnownHostsStore,
) {
    let Some(parsed) = summary.parsed.as_ref() else {
        return;
    };
    summary
        .signals
        .retain(|s| s.kind != vetter_core::SignalKind::UnknownHost);
    summary
        .signals
        .extend(vetter_core::check_known_hosts(parsed, known_hosts));

    summary.host_known = parsed
        .effects
        .iter()
        .map(|e| match e {
            vetter_core::Effect::HttpRequest(req) => crate::policy::is_known_host(req, known_hosts),
            _ => false,
        })
        .collect();
}

#[cfg(test)]
#[path = "tests/pending.rs"]
mod tests;
