//! Pending-prompt request queue.
//!
//! Mediates between connection workers and the approval UI: a worker
//! that hits a prompt-class outcome calls [`PendingQueue::submit`],
//! gets a [`Receiver`], and blocks on it. The UI delegate (real
//! [`crate::notifier::Notifier`] or [`crate::notifier::MockNotifier`])
//! calls [`PendingQueue::resolve`] from whatever thread the platform
//! gives it; the worker wakes with a [`PendingDecision`] and writes the
//! wire reply.
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

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

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
    sender: Sender<PendingDecision>,
}

/// Thread-safe map of `id → (summary, sender)`. Owned by the daemon
/// [`crate::Context`].
#[derive(Default)]
pub struct PendingQueue {
    inner: Mutex<HashMap<String, Entry>>,
}

/// Reason filed in the audit log when the daemon shuts down with a
/// connection still parked on `submit`. Single source of truth so
/// tests can grep on it.
pub const SHUTDOWN_REASON: &str = "daemon shutting down";

impl PendingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new prompt-class request. The returned [`Receiver`]
    /// blocks until the matching id is resolved or the queue is
    /// cancelled (see [`Self::cancel_all`]). The caller should treat
    /// `Err(RecvError)` as a deny.
    ///
    /// Duplicate ids overwrite the prior entry — by construction
    /// every request carries a fresh ULID, so this is a defensive
    /// fallback rather than a normal path.
    pub fn submit(&self, summary: PromptSummary) -> Receiver<PendingDecision> {
        let (tx, rx) = channel();
        let mut g = self.inner.lock().expect("pending mutex poisoned");
        g.insert(
            summary.id.clone(),
            Entry {
                summary,
                sender: tx,
            },
        );
        rx
    }

    /// Post a decision for `id`. Returns `true` if a waiter was
    /// notified, `false` if the id was unknown (e.g. already resolved
    /// or never submitted). Callers can use the return value to log
    /// stale clicks.
    pub fn resolve(&self, id: &str, decision: PendingDecision) -> bool {
        let entry = {
            let mut g = self.inner.lock().expect("pending mutex poisoned");
            g.remove(id)
        };
        match entry {
            Some(e) => e.sender.send(decision).is_ok(),
            None => false,
        }
    }

    /// Drop every outstanding entry. Receivers wake with
    /// `Err(RecvError)`. Used on daemon shutdown so blocked workers
    /// don't hold connections open past the SIGTERM.
    pub fn cancel_all(&self) {
        let mut g = self.inner.lock().expect("pending mutex poisoned");
        g.clear();
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("pending mutex poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Snapshot of currently-pending summaries. Cheap clone of the
    /// stored values; the future menu-bar popover reads through this.
    pub fn pending_summaries(&self) -> Vec<PromptSummary> {
        self.inner
            .lock()
            .expect("pending mutex poisoned")
            .values()
            .map(|e| e.summary.clone())
            .collect()
    }
}

#[cfg(test)]
#[path = "tests/pending.rs"]
mod tests;
