//! The GTK-specific half of the window's lowering.
//!
//! Most of what this module used to hold now lives in
//! [`crate::cards`], where the macOS popover can reach it too: the
//! card view-model, the effect rows, the resolved-card attribution
//! and the picker rows are the same decisions on both platforms, and
//! keeping two copies is how they drift.
//!
//! What stays here is what is genuinely *this surface's*:
//!
//! - **Audit reasons.** `approved via window` is not
//!   `approved via popover`, `approved via notification` or
//!   `approved via admin socket`. The distinction is the audit
//!   trail's record of which surface a human actually used, so these
//!   must not be unified.
//! - **Disclosure state.** [`ExpandedState`] exists because
//!   [`super::window::refresh`] rebuilds every card from scratch; the
//!   macOS popover does not rebuild and has no use for it.
//! - **Pango markup.** [`spans_to_markup`] is the one place a
//!   concrete colour value appears, because Pango takes hex strings.
//!   The palette is injected rather than constant, since only the GTK
//!   thread can ask which theme is active — which also lets the
//!   markup *structure* be tested against a fixed palette with no
//!   display.
//!
//! Everything here is free of `gtk4` and reachable from a test with
//! no display, no session bus and no main loop, which is the only
//! automated coverage the window can have (`plans/LinuxApp.md` §6g).

use std::collections::HashSet;

use crate::cards::{card::CardView, markup::escape_markup, resolved::ResolvedCardView, spans};
use crate::pending::PendingDecision;

/// Audit reasons written when a decision comes from the window.
///
/// Same shape as Phase 6a's `… via admin socket` and 6b's
/// `… via notification`, and the strings `plans/LinuxApp.md` §7
/// step 13 expects to find interleaved in the log.
pub(crate) const REASON_APPROVED: &str = "approved via window";
pub(crate) const REASON_REJECTED: &str = "rejected via window";

/// The two things a card's buttons can do. Named rather than passing
/// a bare `bool` so the call site reads as a decision instead of a
/// flag.
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

/// What the window shows when it has nothing to approve.
///
/// Kept on the GTK side rather than shared: the body names the *tray
/// menu*, which is a Linux surface macOS does not have (it has a
/// menu-bar item and its own wording). Sharing the copy would mean
/// one platform lying about the other's affordances.
pub(crate) const EMPTY_TITLE: &str = "No pending approvals";
pub(crate) const EMPTY_BODY: &str =
    "Requests that need a decision will appear here. You can also approve \
     from the tray menu, a notification, or `vet daemon approve`.";

/// Header shown above the resolved section.
pub(crate) const RECENT_TITLE: &str = "Recent";
/// Placeholder standing in for the pending block when it is empty but
/// Recent is not. Without it the window opens straight onto resolved
/// cards with no action buttons and no explanation.
pub(crate) const NO_PENDING: &str = "No pending approvals.";

// ── Disclosure state ────────────────────────────────────────────────────────

/// Which collapsible section of a card a disclosure drives.
///
/// Three kinds rather than three `HashSet` fields threaded through
/// three near-identical accessor pairs: the widget side just wants
/// "is *this* disclosure on *this* card open", and naming the kinds
/// keeps that a single call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Disclosure {
    /// "Show raw" — the §8.5 body. On every card.
    Raw,
    /// "Details" — the structured effect rows. Resolved cards only:
    /// a pending card shows its rows inline, because someone
    /// deciding *now* should not have to go looking for what the
    /// command does.
    Details,
    /// "See approval reason" — rule attribution plus the revoke
    /// button. Auto-allowed resolved cards only.
    Reason,
}

/// Which collapsible sections of which cards are currently open.
///
/// This lives in the model rather than in the `Expander` widgets for
/// a reason worth spelling out: [`super::window::refresh`] rebuilds
/// every card from scratch, and it runs on *every* queue change —
/// including changes that have nothing to do with the card the user
/// is reading. A notification approved on another card, a
/// `vet daemon approve` over the admin socket, a rule addition that
/// auto-resolves something: any of those would collapse an open
/// disclosure mid-read if the open/closed bit lived in the widget
/// that gets destroyed.
///
/// Keyed by `(kind, request ULID)`, so the state follows the card
/// rather than its position in the list, and a card's three
/// disclosures stay independent of one another.
#[derive(Debug, Default)]
pub(crate) struct ExpandedState {
    /// Absent means closed, which is the default the spec asks for
    /// ("Default state: collapsed").
    open: HashSet<(Disclosure, String)>,
}

impl ExpandedState {
    pub(crate) fn is_open(&self, kind: Disclosure, id: &str) -> bool {
        // `HashSet::contains` over an owned key pair: the tuple would
        // need a matching `Borrow` impl to probe by reference, which
        // is not worth a newtype for a set this small (bounded by the
        // number of cards on screen).
        self.open.contains(&(kind, id.to_string()))
    }

    pub(crate) fn set_open(&mut self, kind: Disclosure, id: &str, open: bool) {
        let key = (kind, id.to_string());
        if open {
            self.open.insert(key);
        } else {
            self.open.remove(&key);
        }
    }

    /// Forget ids the window is no longer showing.
    ///
    /// Without this the set grows for the lifetime of the daemon:
    /// every request whose disclosure was ever opened would leave a
    /// ULID behind after it fell out of view. The live set spans
    /// *both* sections — a resolved card is still on screen, so
    /// pruning against pending ids alone would collapse the Recent
    /// disclosure the user just opened.
    pub(crate) fn retain_live(&mut self, live_ids: &[&str]) {
        self.open
            .retain(|(_, id)| live_ids.iter().any(|live| live == id));
    }
}

/// Every id currently on screen, across both sections.
///
/// Stays beside [`ExpandedState`] rather than moving to
/// [`crate::cards`]: it exists only to feed
/// [`ExpandedState::retain_live`], and the macOS popover — which does
/// not rebuild — has nothing to prune.
pub(crate) fn live_ids<'a>(
    pending: &'a [CardView],
    resolved: &'a [ResolvedCardView],
) -> Vec<&'a str> {
    pending
        .iter()
        .map(|c| c.id.as_str())
        .chain(resolved.iter().map(|r| r.card.id.as_str()))
        .collect()
}

// ── Markup ──────────────────────────────────────────────────────────────────

/// Concrete colours for [`spans_to_markup`].
///
/// Pango markup takes hex strings, so this is the one place a colour
/// value has to appear. It is a parameter rather than a constant
/// because only the GTK thread can ask which theme is active, and the
/// palette that reads well on a dark surface is illegible on a light
/// one. Keeping it injected also means the markup *structure* can be
/// tested against a fixed palette without a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkupPalette {
    pub red: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub magenta: &'static str,
    pub cyan: &'static str,
    pub blue: &'static str,
    /// Used for bright-black and for the dimmed-cyan loopback run.
    pub dim: &'static str,
}

/// Render parsed ANSI spans as Pango markup.
///
/// **Every span's text is escaped** ([`escape_markup`]) before it is
/// wrapped in a tag. The text comes from argv; without escaping, a
/// URL containing `<span foreground='…'>` would be parsed as markup
/// on the surface a human reads to decide whether to approve the
/// command — it could recolour or hide the very text describing what
/// is about to run. This is the same hole the notification
/// `body-markup` path closes, which is why both call the same escape.
///
/// Spans carrying no style emit bare escaped text rather than an
/// empty `<span>`, which keeps the markup readable in a test failure.
pub(crate) fn spans_to_markup(spans: &[spans::AnsiSpan], palette: MarkupPalette) -> String {
    let mut out = String::new();
    for span in spans {
        let text = escape_markup(&span.text);
        let mut attrs = String::new();
        if let Some(colour) = span_colour(span.style, palette) {
            attrs.push_str(&format!(" foreground=\"{colour}\""));
        }
        if span.style.bold {
            attrs.push_str(" weight=\"bold\"");
        }
        if span.style.underline {
            attrs.push_str(" underline=\"single\"");
        }
        if attrs.is_empty() {
            out.push_str(&text);
        } else {
            out.push_str(&format!("<span{attrs}>{text}</span>"));
        }
    }
    out
}

/// Resolve one span's colour against the palette.
///
/// Two cases are not a straight table lookup, and both come from
/// `plans/ApprovalUI.md` "Body colouring":
/// - cyan + dim is the loopback style (`2;36`), which reads as muted
///   rather than as the teal used for a live URL;
/// - bright-black is the renderer's "secondary" colour, so it maps to
///   the same dim value.
fn span_colour(style: spans::SpanStyle, palette: MarkupPalette) -> Option<&'static str> {
    use spans::AnsiColor;
    match style.color? {
        AnsiColor::Cyan if style.dim => Some(palette.dim),
        AnsiColor::Cyan => Some(palette.cyan),
        AnsiColor::Red => Some(palette.red),
        AnsiColor::Green => Some(palette.green),
        AnsiColor::Yellow => Some(palette.yellow),
        AnsiColor::Magenta => Some(palette.magenta),
        AnsiColor::BrightBlack => Some(palette.dim),
        AnsiColor::BrightBlue => Some(palette.blue),
    }
}

#[cfg(test)]
#[path = "../../tests/window_model.rs"]
mod tests;
