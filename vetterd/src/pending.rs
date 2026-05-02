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

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use vetter_core::wire::WireDecision;

/// Compact one-line UI summary of a prompt-class request. Built by
/// the daemon from the re-parsed [`vetter_core::ParsedCommand`] and
/// passed verbatim into the notifier so every UI surface (real
/// notification, mock control socket, future popover detail view)
/// renders the same fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    listener: Option<ChangeListener>,
}

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
    pub fn submit(&self, summary: PromptSummary) -> Receiver<PendingDecision> {
        self.submit_with_render(summary, String::new())
    }

    /// Register a new prompt-class request with its pre-rendered §8.5
    /// detail. The returned [`Receiver`] blocks until the matching id
    /// is resolved or the queue is cancelled (see
    /// [`Self::cancel_all`]). The caller should treat
    /// `Err(RecvError)` as a deny.
    ///
    /// Duplicate ids overwrite the prior entry — by construction
    /// every request carries a fresh ULID, so this is a defensive
    /// fallback rather than a normal path.
    pub fn submit_with_render(
        &self,
        summary: PromptSummary,
        rendered: String,
    ) -> Receiver<PendingDecision> {
        let (tx, rx) = channel();
        let listener = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            g.entries.insert(
                summary.id.clone(),
                Entry {
                    summary,
                    rendered,
                    sender: tx,
                },
            );
            g.listener.clone()
        };
        if let Some(cb) = listener {
            cb();
        }
        rx
    }

    /// Post a decision for `id`. Returns `true` if a waiter was
    /// notified, `false` if the id was unknown (e.g. already resolved
    /// or never submitted). Callers can use the return value to log
    /// stale clicks. The change listener fires only when an entry
    /// was actually removed; spurious clicks don't redraw the UI.
    pub fn resolve(&self, id: &str, decision: PendingDecision) -> bool {
        let (entry, listener) = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            let removed = g.entries.remove(id);
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

    /// Drop every outstanding entry. Receivers wake with
    /// `Err(RecvError)`. Used on daemon shutdown so blocked workers
    /// don't hold connections open past the SIGTERM.
    /// Fires the change listener iff the queue was non-empty.
    pub fn cancel_all(&self) {
        let (had_entries, listener) = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            let had = !g.entries.is_empty();
            g.entries.clear();
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

#[cfg(test)]
#[path = "tests/pending.rs"]
mod tests;
