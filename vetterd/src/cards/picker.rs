//! View-models for the "Allowlist…" and "Trust host…" pickers.
//!
//! The picker is the one place an approval surface *writes* to the
//! allowlist, so the shape it presents matters more than most: each
//! row pairs the suggestion's headline with the exact YAML that
//! would be persisted. A picker that wrote a rule the user never
//! read would be worse than no picker, which is why
//! [`super::rules::render_rule_when_yaml`] is part of the row rather
//! than an optional flourish.

use vetter_core::matcher::Rule;
use vetter_core::suggest::{HostSuggestion, RuleSuggestion};
use vetter_core::{RiskSignal, SignalKind};

use super::rules::{self, DurationChoice};

/// Which of the two pickers a surface is asking for.
///
/// Exists because a picker can now be requested from somewhere that
/// cannot draw it. A notification action and a tray item both mean
/// "open this picker on that card", and the window is what actually
/// opens it, so the request has to survive as *data* on the way
/// across. macOS reaches the same two sheets from its banner, where
/// the equivalent distinction is the action identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// "Allowlist…" — pick a generalisation tier and a duration.
    Allowlist,
    /// "Trust host…" — mark the host known. Only meaningful while
    /// [`show_trust_host`] holds for the card.
    TrustHost,
}

/// One radio row in a picker: the tier headline and the YAML the user
/// would be persisting.
///
/// `preview` is a plain string destined for a plain text widget. It
/// is built from argv-derived data, and the one thing it must never
/// become is markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub title: String,
    pub preview: String,
}

/// Rows for the "Allowlist…" picker, tightest tier first (the order
/// [`vetter_core::suggest::allowlist_suggestions`] emits).
pub fn rule_picker_rows(suggestions: &[RuleSuggestion]) -> Vec<PickerRow> {
    suggestions
        .iter()
        .map(|s| PickerRow {
            title: format!("{}  —  {}", s.tier.as_str(), s.label),
            preview: rules::render_rule_when_yaml(&s.rule),
        })
        .collect()
}

/// Rows for the "Trust host…" picker.
pub fn host_picker_rows(suggestions: &[HostSuggestion]) -> Vec<PickerRow> {
    suggestions
        .iter()
        .map(|s| PickerRow {
            title: format!("{}  —  {}", s.tier.as_str(), s.label),
            preview: format!("pattern: \"{}\"", s.entry.pattern),
        })
        .collect()
}

/// Stamp a duration choice onto a suggested rule.
///
/// The whole reason [`DurationChoice`] lives in the shared layer: a
/// 15-minute rule must expire in 15 minutes and a session rule must
/// carry the requesting terminal's sid, whether the radio that
/// selected it was an `NSButton` or a `GtkCheckButton`.
pub fn rule_with_duration(
    mut rule: Rule,
    choice: DurationChoice,
    now: u64,
    peer_sid: Option<i32>,
) -> Rule {
    let (expires_at, sid) = choice.apply(now, peer_sid);
    rule.expires_at = expires_at;
    rule.sid = sid;
    rule
}

/// Whether the "Trust host…" button should appear for these signals.
///
/// Gated on `UnknownHost` being present, mirroring the
/// `host_suggestions` engine contract: when the host is already
/// trusted there is nothing to suggest, and a button that opens an
/// empty picker is worse than no button.
pub fn show_trust_host(signals: &[RiskSignal]) -> bool {
    signals.iter().any(|s| s.kind == SignalKind::UnknownHost)
}

#[cfg(test)]
#[path = "../tests/cards_picker.rs"]
mod tests;
