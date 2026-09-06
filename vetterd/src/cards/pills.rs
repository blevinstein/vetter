//! Risk signals → pill tone, label, tooltip, and sort order.
//!
//! A "pill" is the small tinted chip an approval surface paints for
//! each risk signal on a card. What the chip *says* and how urgent
//! it reads are policy decisions shared by every platform; only the
//! rounded-rectangle drawing is toolkit work.
//!
//! [`signal_pill`] answers "what chip, if any, does this signal
//! get?" and [`signal_priority`] answers "where in the row does it
//! land?". They live side by side deliberately: the two must stay in
//! lockstep, and re-tiering a signal without reordering the row
//! produces a chip that reads urgent but sorts last.
//!
//! See [plans/ApprovalUI.md "Element catalogue"](../../../plans/ApprovalUI.md)
//! for the rendered result.

use vetter_core::render::signal_kind_label;
use vetter_core::{BadgeSeverity, SignalKind};

/// Semantic urgency of a pill. Each platform maps a tone onto its
/// own palette — deliberately *not* a colour, so the macOS popover
/// can say `NSColor::systemRedColor()` and a GTK card can say a
/// theme class without either dictating to the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Red. `BadgeSeverity::Danger`.
    Danger,
    /// Orange. `BadgeSeverity::Warn`, excluding `AuthHeader`.
    Warn,
    /// Green. Today only `AuthHeader` — see [`signal_pill`].
    Positive,
}

/// Everything a surface needs to paint one signal pill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PillSpec {
    pub tone: Tone,
    /// Chip text, from [`vetter_core::render::signal_kind_label`].
    pub label: &'static str,
    /// Hover text: the label, plus `RiskSignal::detail` when the
    /// analyzer supplied one.
    pub tooltip: String,
}

/// Describe the pill for `kind`, using `detail` for the tooltip.
///
/// Returns `None` for `Info`-tier kinds — those lines stay in the
/// §8.5 raw body inside the "Show raw" disclosure rather than
/// earning a chip, so callers can `.flatten()` over the iterator
/// without an explicit branch.
///
/// `AuthHeader` is special-cased to a *positive* tone: an auth
/// header on a request is generally a good sign (the agent has
/// credentials and we redacted them on the way in), not the "watch
/// out, secret on the wire" alarm the orange Warn tier was reading
/// as. Other Warn-tier signals stay orange. The vetter-core
/// `ui_severity` classifier is left unchanged so the analyzer / CLI
/// still treat `AuthHeader` as a signal worth surfacing in the
/// body's `Risk signals:` list.
pub fn signal_pill(kind: SignalKind, detail: &str) -> Option<PillSpec> {
    let tone = signal_tone(kind)?;
    let label = signal_kind_label(kind);
    let tooltip = if detail.is_empty() {
        label.to_string()
    } else {
        format!("{label}: {detail}")
    };
    Some(PillSpec {
        tone,
        label,
        tooltip,
    })
}

/// Tone for `kind`, or `None` for `Info`-tier kinds that get no
/// pill at all. Split out from [`signal_pill`] so
/// [`signal_priority`] can share the same classification without
/// building a tooltip string it would throw away.
pub fn signal_tone(kind: SignalKind) -> Option<Tone> {
    if kind == SignalKind::AuthHeader {
        return Some(Tone::Positive);
    }
    match kind.ui_severity() {
        BadgeSeverity::Danger => Some(Tone::Danger),
        BadgeSeverity::Warn => Some(Tone::Warn),
        BadgeSeverity::Info => None,
    }
}

/// Triage priority for sorting signal pills left-to-right inside the
/// pills row. Lower number sorts first (closer to the start of the
/// row), so the user's eye lands on the most-urgent chips before
/// scanning past the supportive ones:
///
/// | Priority | Tone                                  | Colour |
/// |---------:|---------------------------------------|--------|
/// |       0  | `Danger`                              | red    |
/// |       1  | `Warn` (excluding `AuthHeader`)       | orange |
/// |       2  | `Positive` (`AuthHeader`)             | green  |
/// |       3  | no pill (`Info`) — sorted last regardless |    |
///
/// Derived from [`signal_tone`] so the "what colour is this kind?"
/// and "where in the row does it land?" answers cannot drift apart:
/// flipping a signal's tone reorders the row in the same edit.
pub fn signal_priority(kind: SignalKind) -> u8 {
    match signal_tone(kind) {
        Some(Tone::Danger) => 0,
        Some(Tone::Warn) => 1,
        Some(Tone::Positive) => 2,
        None => 3,
    }
}

#[cfg(test)]
#[path = "../tests/cards_pills.rs"]
mod tests;
