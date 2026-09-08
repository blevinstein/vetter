//! Entries from the resolved-history ring, lowered for painting.
//!
//! A resolved card is a [`super::card::CardView`] plus the answer to
//! two questions the pending section never asks: how did this come
//! out, and — when the matcher allowed it without a human — which
//! rule did that, and can the user take it back?

use vetter_core::matcher::Scope;
use vetter_core::wire::{WireDecision, WireScope};

use super::card::{card_view, CardView};
use crate::pending::ResolvedEntry;

/// How a resolved request came out.
///
/// Narrower than [`vetter_core::wire::WireDecision`] on purpose: a
/// surface paints two outcomes, and `AllowOnce` — which no daemon
/// emits today — reads as an allow rather than as a third badge
/// nobody has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allowed,
    Denied,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Allowed => "allowed",
            Outcome::Denied => "denied",
        }
    }
}

/// Why a resolved card was auto-allowed, and whether the surface can
/// take the rule back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleAttribution {
    pub rule_id: String,
    pub scope: Scope,
    /// Sentence shown when "See approval reason" is opened.
    pub summary: String,
    /// `Some(scope)` when "Revoke rule" should be offered and where
    /// the removal would land; `None` when this layer is not editable
    /// from a UI surface — see [`revoke_scope`].
    pub revoke: Option<WireScope>,
}

/// One entry from the resolved-history ring, lowered for painting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCardView {
    /// Same shape as a pending card, so both sections share every row
    /// builder. Only the chrome around it differs.
    pub card: CardView,
    pub outcome: Outcome,
    /// `Some` only for auto-allowed entries the matcher attributed to
    /// a rule. Drives "See approval reason" / "Revoke rule".
    pub attribution: Option<RuleAttribution>,
    /// Whether the picker row appears. Allow-resolved cards keep it
    /// (the user may still want a standing rule); deny-resolved cards
    /// drop it — offering "Allowlist…" immediately after someone
    /// pressed Reject reads as arguing with them.
    pub show_pickers: bool,
}

/// Which allowlist layer a revoke would write to, or `None` when a UI
/// surface cannot edit it.
///
/// Session-scoped rules live in the *same* on-disk user file as any
/// other rule (the loader partitions them at load time), so
/// `Scope::Session` is revokable through the identical `User` remove
/// call rather than being a dead end. Built-in, project and denylist
/// entries are not editable here: project rules belong to
/// `vet allow rm`, and the other two are not user-owned at all.
pub fn revoke_scope(scope: Scope) -> Option<WireScope> {
    match scope {
        Scope::User | Scope::Session => Some(WireScope::User),
        Scope::Project | Scope::Builtin | Scope::Denylist => None,
    }
}

/// Lower one resolved-history entry into a card.
pub fn resolved_card_view(entry: &ResolvedEntry) -> ResolvedCardView {
    let outcome = match entry.decision {
        WireDecision::Allow | WireDecision::AllowOnce => Outcome::Allowed,
        WireDecision::Deny => Outcome::Denied,
    };
    // Attribution needs both halves. `rule_scope` is documented as
    // always populated alongside `rule_id`, but zipping rather than
    // unwrapping means a future producer that sets only one cannot
    // panic a UI thread.
    let attribution = entry
        .rule_id
        .as_ref()
        .zip(entry.rule_scope)
        .filter(|_| outcome == Outcome::Allowed)
        .map(|(rule_id, scope)| RuleAttribution {
            rule_id: rule_id.clone(),
            scope,
            summary: format!(
                "Auto-allowed by rule `{rule_id}` in {} scope.",
                scope.as_str()
            ),
            revoke: revoke_scope(scope),
        });

    ResolvedCardView {
        card: card_view(&entry.summary, &entry.rendered),
        outcome,
        attribution,
        show_pickers: outcome == Outcome::Allowed,
    }
}

/// Lower the resolved-history ring for painting.
///
/// Order is left exactly as the queue holds it — newest first, since
/// [`crate::pending::PendingQueue::resolve`] pushes to the front.
/// That is the opposite of the pending section's oldest-first sort,
/// and deliberately so: pending answers "what has been waiting
/// longest", Recent answers "what just happened".
pub fn resolved_snapshot(resolved: &[ResolvedEntry]) -> Vec<ResolvedCardView> {
    resolved.iter().map(resolved_card_view).collect()
}

#[cfg(test)]
#[path = "../tests/cards_resolved.rs"]
mod tests;
