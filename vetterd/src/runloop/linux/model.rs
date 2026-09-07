//! Pure lowering from queue state to what the window paints.
//!
//! Deliberately free of `gtk4`: everything here is reachable from a
//! test with no display, no session bus, and no main loop, which is
//! the only automated coverage the window can have (CI has no
//! display, `plans/LinuxApp.md` §6g). The widget assembly in
//! [`super::window`] is a thin translation of these values into
//! `gtk4` objects and owns no decisions of its own.
//!
//! Same split the macOS side arrived at, and the reason
//! `crate::cards` exists: URL segmentation, signal tones and host
//! trust are already lowered and unit-tested there, so nothing in
//! this module re-derives them.

use crate::cards;
use crate::pending::{PendingDecision, PromptSummary};

/// Audit reasons written when a decision comes from the window.
///
/// Same shape as Phase 6a's `… via admin socket` and 6b's
/// `… via notification`, and the strings `plans/LinuxApp.md` §7
/// step 13 expects to find interleaved in the log.
pub(crate) const REASON_APPROVED: &str = "approved via window";
pub(crate) const REASON_REJECTED: &str = "rejected via window";

/// The two things a card's buttons can do. Named rather than passing
/// a bare `bool` so the call site reads as a decision instead of a
/// flag, and so step 3's picker actions have somewhere to land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardAction {
    Approve,
    Reject,
}

impl CardAction {
    /// Lower a button press into the decision the queue records.
    pub(crate) fn decision(self) -> PendingDecision {
        match self {
            CardAction::Approve => PendingDecision::allow(REASON_APPROVED),
            CardAction::Reject => PendingDecision::deny(REASON_REJECTED),
        }
    }
}

/// One pending request, lowered to the strings and tones the window
/// paints.
///
/// Step 1 carries only what a minimal card needs. The §8.5 detail
/// disclosure, raw-command disclosure, signal pills and per-effect
/// rows arrive in step 2 and hang off the same `summary`-derived
/// shape — `crate::cards` already produces all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardView {
    /// Correlation ULID. The identity the buttons resolve against.
    pub id: String,
    /// Title line, e.g. `curl` or `curl (dry run)`.
    pub title: String,
    /// Target line: `GET https://example.com/`, already sanitised.
    pub target: String,
    /// Host-trust classification for the request's primary host,
    /// lowered by `cards::url` so the window and the macOS popover
    /// agree on what counts as trusted.
    pub trust: cards::url::HostTrust,
}

/// What the window shows when it has nothing to approve. Kept here
/// rather than inline in the widget code so the empty state is
/// covered by a test like every other state.
pub(crate) const EMPTY_TITLE: &str = "No pending approvals";
pub(crate) const EMPTY_BODY: &str =
    "Requests that need a decision will appear here. You can also approve \
     from the tray menu, a notification, or `vet daemon approve`.";

/// Lower one summary into its card.
///
/// Every argv-derived string goes through
/// [`vetter_core::render::sanitize_for_display`] first — a hostile
/// URL carrying RTLO or zero-width bytes would otherwise be painted
/// verbatim onto the surface a human uses to authorise it. Same
/// defence the notification body and the tray menu label apply.
pub(crate) fn card_view(summary: &PromptSummary) -> CardView {
    use vetter_core::render::sanitize_for_display;

    let command = sanitize_for_display(&summary.command);
    let verb = sanitize_for_display(&summary.primary_verb);
    let target = sanitize_for_display(&summary.primary_target);

    let title = if summary.force_prompt {
        format!("{command} (dry run)")
    } else {
        command.into_owned()
    };
    let target = if summary.primary_verb.is_empty() {
        target.into_owned()
    } else {
        format!("{verb} {target}")
    };

    CardView {
        id: summary.id.clone(),
        title,
        target,
        trust: trust_of(summary),
    }
}

/// Host-trust for the summary's primary host.
///
/// `host_known` is computed daemon-side against the known-hosts store
/// (the window cannot reach it), so entry 0 — the primary HTTP
/// effect — is what the pill reflects. Falls back to "unknown" when
/// the summary predates that field or carries no HTTP effect, which
/// is the conservative direction: an unknown-host pill overstates
/// risk, a known-host pill would understate it.
fn trust_of(summary: &PromptSummary) -> cards::url::HostTrust {
    let host = summary
        .parsed
        .as_ref()
        .and_then(|p| p.effects.first())
        .and_then(|e| match e {
            vetter_core::Effect::HttpRequest(req) => req.url.host_str(),
            _ => None,
        });
    match host {
        Some(h) => cards::url::host_trust(h, summary.host_known.first().copied().unwrap_or(false)),
        None => cards::url::HostTrust::Unknown,
    }
}

/// Snapshot the queue's pending entries into cards, oldest first.
///
/// ULIDs are time-sortable, so a plain sort by id is chronological.
/// Oldest-first is deliberate and differs from the tray's newest-last
/// ordering only in framing: the request that has been blocking an
/// agent longest is the one the user should answer first, and it sits
/// at the top where it is reachable without scrolling.
pub(crate) fn snapshot(pending: &[PromptSummary]) -> Vec<CardView> {
    let mut cards: Vec<CardView> = pending.iter().map(card_view).collect();
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    cards
}

#[cfg(test)]
#[path = "../../tests/window_model.rs"]
mod tests;
