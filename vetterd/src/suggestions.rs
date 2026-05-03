//! Phase-5 suggestion handlers: persist allowlist rules / known-host
//! entries, reload the in-memory stores, and side-effect the
//! pending queue when an allowlist change auto-approves work that
//! was waiting for human review.
//!
//! Three entry points feed the admin socket
//! ([`crate::handle_admin_request`]):
//!
//! - [`suggestions_for`] builds the picker payload for a given
//!   request id (pending or recently-resolved).
//! - [`add_allowlist_rule`] persists a rule to YAML, reloads the
//!   `AllowlistStore`, then iterates pending entries to auto-resolve
//!   any whose new decision is `Allow`. Returns the persisted id +
//!   the list of auto-approved request ids.
//! - [`add_known_host`] persists a known-host entry, reloads the
//!   `KnownHostsStore`, and refreshes the pending queue's signal
//!   cache — covering both pending entries and the Recent
//!   (resolved-history) ring — so the popover repaints (host pill
//!   flips orange → green; `UnknownHost` signal disappears).
//!
//! ## Lock ordering
//!
//! The store write locks are taken **after** the disk write so a
//! YAML load failure doesn't poison the in-memory copy. While the
//! write lock is held we never call back into `pending.resolve` —
//! that gets fired only after the lock is dropped, so a concurrent
//! connection worker reading the store can run interleaved with
//! the auto-approve loop.
//!
//! ## Scope wiring (v1)
//!
//! [`crate::WireScope::User`] writes to `~/.vet/allowlist.yaml`
//! (or [`crate::Context::allowlist_override`] when set, so
//! integration tests can sandbox writes inside a tempdir).
//! [`crate::WireScope::Project`] is rejected with an
//! `unsupported_scope` error in this PR — the popover only offers
//! the user scope, and the existing `vet allow add --scope project`
//! CLI already covers the project case from outside the daemon.

use std::path::PathBuf;
use std::sync::Arc;

use vetter_core::known_hosts::{self, KnownHostEntry};
use vetter_core::matcher::loader as allow_loader;
use vetter_core::matcher::{decide, derive_auto_id, Decision, Rule};
use vetter_core::suggest::{
    allowlist_suggestions, host_suggestions, HostSuggestion, RuleSuggestion,
};
use vetter_core::wire::WireScope;

use crate::pending::PendingDecision;
use crate::Context;

/// Result of a successful [`add_allowlist_rule`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedRule {
    /// The id under which the rule was persisted (auto-derived if
    /// the caller supplied an empty string).
    pub id: String,
    /// Pending request ids that were auto-approved by this add.
    /// Empty when the new rule did not cover any pending entry.
    pub auto_approved_ids: Vec<String>,
}

/// Reasons an admin write can fail.
///
/// Wraps the loader / known-hosts errors and adds an
/// `UnsupportedScope` variant for the `Project` placeholder. Carries
/// `Display` so the admin loop can stringify it into
/// `MgmtResponse::Error`.
#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error("unsupported scope `{0:?}` (only `User` is wired through the popover today)")]
    UnsupportedScope(WireScope),
    #[error("$HOME is not set; cannot resolve user-scope path")]
    NoUserHome,
    #[error("allowlist write: {0}")]
    Allowlist(#[from] allow_loader::LoadError),
    #[error("known-hosts write: {0}")]
    KnownHosts(#[from] known_hosts::KnownHostsError),
}

/// Build the allowlist + known-host suggestion lists for `id`.
///
/// Searches the pending queue first, then the resolved-history
/// ring. Returns `None` when the id is unknown or when the
/// resolved entry was a deny (suggestions for denied requests would
/// be confusing — the user just rejected this).
pub fn suggestions_for(
    ctx: &Arc<Context>,
    id: &str,
) -> Option<(Vec<RuleSuggestion>, Vec<HostSuggestion>)> {
    let (pending, resolved) = ctx.pending.all_entries();
    let parsed = pending
        .iter()
        .find_map(|(s, _)| (s.id == id).then(|| s.parsed.clone()))
        .flatten()
        .or_else(|| {
            resolved.iter().find_map(|entry| {
                use vetter_core::wire::WireDecision::*;
                if entry.summary.id != id {
                    return None;
                }
                match entry.decision {
                    Allow | AllowOnce => entry.summary.parsed.clone(),
                    Deny => None,
                }
            })
        })?;

    let known_hosts = ctx.known_hosts.read().expect("known_hosts lock poisoned");
    Some((
        allowlist_suggestions(&parsed),
        host_suggestions(&parsed, &known_hosts),
    ))
}

/// Persist `rule` to the indicated allowlist scope, reload the
/// in-memory store, and auto-resolve every pending request whose
/// new decision is `Allow`. Returns the persisted id and the list
/// of auto-approved request ids.
pub fn add_allowlist_rule(
    ctx: &Arc<Context>,
    scope: WireScope,
    mut rule: Rule,
) -> Result<AddedRule, AddError> {
    if scope != WireScope::User {
        return Err(AddError::UnsupportedScope(scope));
    }
    if rule.id.is_empty() {
        rule.id = derive_auto_id(&rule);
    }
    let id = rule.id.clone();

    let target = resolve_allowlist_path(ctx)?;
    allow_loader::add_rule(&target, rule.clone())?;

    // Reload the layered store from disk under the write lock.
    // Connection workers waiting for a read lock will block briefly
    // (microseconds) and then see the new store on their next
    // evaluate.
    {
        let new_store = allow_loader::load_default(None, ctx.allowlist_override.as_deref())?;
        let mut g = ctx
            .allowlist
            .write()
            .expect("allowlist lock poisoned (write)");
        *g = new_store;
    }

    // Drain pending entries whose parsed command now matches the
    // freshly-added rule. We snapshot summaries while holding only
    // the queue's lock (via `pending_summaries`) so the resolve
    // calls below don't recurse into the queue lock with our own
    // borrows held.
    let store_snapshot = ctx
        .allowlist
        .read()
        .expect("allowlist lock poisoned (read)")
        .clone();
    let summaries = ctx.pending.pending_summaries();
    let mut auto_approved_ids = Vec::new();
    for summary in summaries {
        let Some(parsed) = summary.parsed.as_ref() else {
            continue;
        };
        if let Decision::Allow { rule_id, .. } = decide(parsed, &store_snapshot) {
            if rule_id == id {
                let reason = format!("auto-approved by newly added rule `{id}`");
                if ctx
                    .pending
                    .resolve(&summary.id, PendingDecision::allow(reason))
                {
                    auto_approved_ids.push(summary.id);
                }
            }
        }
    }

    Ok(AddedRule {
        id,
        auto_approved_ids,
    })
}

/// Persist `entry` to the indicated known-hosts scope, reload the
/// in-memory store, and refresh the pending queue so any
/// `UnknownHost` signals on cards that newly become "known" disappear.
pub fn add_known_host(
    ctx: &Arc<Context>,
    scope: WireScope,
    entry: KnownHostEntry,
) -> Result<(), AddError> {
    if scope != WireScope::User {
        return Err(AddError::UnsupportedScope(scope));
    }
    let target = resolve_known_hosts_path(ctx)?;
    known_hosts::add_host(&target, entry)?;

    {
        let new_store = known_hosts::load_default(None)?;
        let mut g = ctx
            .known_hosts
            .write()
            .expect("known_hosts lock poisoned (write)");
        *g = new_store;
    }

    // Re-derive signals + host_known on every pending entry **and**
    // every Recent (resolved-history) entry so the popover repaints
    // both sections at once: a card the user just approved and is
    // staring at drops its stale orange `UnknownHost` pill at the
    // same moment a still-pending card for the same host does. The
    // pending queue's `refresh_with` helper fires the change listener
    // once at the end.
    let store_snapshot = ctx
        .known_hosts
        .read()
        .expect("known_hosts lock poisoned (read)")
        .clone();
    ctx.pending.refresh_with(&store_snapshot);

    Ok(())
}

/// Resolve the on-disk allowlist file for the user scope. When the
/// daemon was started with `--allowlist <path>` (the test escape
/// hatch) we write back to that file rather than the XDG location
/// so the integration test stays sandboxed inside the tempdir.
fn resolve_allowlist_path(ctx: &Arc<Context>) -> Result<PathBuf, AddError> {
    if let Some(p) = &ctx.allowlist_override {
        return Ok(p.clone());
    }
    allow_loader::user_allowlist_path().ok_or(AddError::NoUserHome)
}

/// Resolve the on-disk known-hosts file for the user scope. There
/// is no override env for known-hosts today; tests drive the file
/// path through `$HOME` (`testutil::tmpdir` exports `HOME`).
fn resolve_known_hosts_path(_ctx: &Arc<Context>) -> Result<PathBuf, AddError> {
    known_hosts::user_known_hosts_path().ok_or(AddError::NoUserHome)
}

#[cfg(test)]
#[path = "tests/suggestions.rs"]
mod tests;
