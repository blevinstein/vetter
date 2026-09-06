//! Rule-authoring model: how long an allowlist rule lives, and what
//! it will look like on disk.
//!
//! The "Allowlist…" / "Trust host…" pickers ask the user two
//! questions — *which pattern?* and *for how long?* — and then show
//! the exact YAML they are about to persist. Both the duration
//! ladder ([`DurationChoice`]) and the preview
//! ([`render_rule_when_yaml`]) are policy, not drawing: a 15-minute
//! rule must expire in 15 minutes and a session-scoped rule must
//! carry the requesting terminal's sid whether the radio was an
//! `NSButton` or a `GtkCheckButton`.
//!
//! The preview matters more than it looks. It is the affordance
//! that lets a human see the pattern *before* it is written — a
//! picker that persists a rule you never read would be a worse
//! security surface than one that asks twice. Every platform gets
//! the same string.
//!
//! See [plans/ApprovalUI.md "Allowlist picker"](../../../plans/ApprovalUI.md)
//! for the rendered sheet.

use vetter_core::matcher::Rule;

/// Duration choices offered by the picker's second radio group.
/// Order here is the display order (top to bottom) and the default
/// selection is [`DurationChoice::Forever`] — this preserves the
/// pre-existing one-click muscle memory of "pick a tier, hit the
/// button, done" for anyone who never touches the duration group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationChoice {
    FifteenMinutes,
    OneHour,
    FourHours,
    /// Scoped to the requesting connection's stable POSIX session
    /// (see [`vetter_core::peer_cred::stable_session_for`]).
    /// Disabled in the UI when the card never resolved a `peer_sid`.
    ThisSession,
    Forever,
}

impl DurationChoice {
    pub const ALL: [DurationChoice; 5] = [
        DurationChoice::FifteenMinutes,
        DurationChoice::OneHour,
        DurationChoice::FourHours,
        DurationChoice::ThisSession,
        DurationChoice::Forever,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            DurationChoice::FifteenMinutes => "15 minutes",
            DurationChoice::OneHour => "1 hour",
            DurationChoice::FourHours => "4 hours",
            DurationChoice::ThisSession => "For this terminal session",
            DurationChoice::Forever => "Forever",
        }
    }

    /// Backstop TTL applied to `Some(sid)`-scoped rules purely so
    /// `add_rule`'s lazy prune (see
    /// `vetter_core::matcher::loader::add_rule`) eventually reaps
    /// them even if the terminal that set the SID never comes back
    /// to invalidate the match. Does not change matching behaviour
    /// — the `sid` check already stops the rule from matching well
    /// before this elapses — it just bounds how long a dead entry
    /// can sit physically in the YAML file.
    const SESSION_BACKSTOP_SECS: u64 = 7 * 24 * 60 * 60;

    /// Compute the `(expires_at, sid)` pair to write onto the
    /// [`Rule`] for this choice, given the current wall clock and
    /// the card's recorded `peer_sid`. Only [`DurationChoice::ThisSession`]
    /// reads `peer_sid`; every other variant ignores it.
    pub fn apply(&self, now: u64, peer_sid: Option<i32>) -> (Option<u64>, Option<i32>) {
        match self {
            DurationChoice::FifteenMinutes => (Some(now + 15 * 60), None),
            DurationChoice::OneHour => (Some(now + 60 * 60), None),
            DurationChoice::FourHours => (Some(now + 4 * 60 * 60), None),
            DurationChoice::ThisSession => (Some(now + Self::SESSION_BACKSTOP_SECS), peer_sid),
            DurationChoice::Forever => (None, None),
        }
    }

    pub fn confirmation_note(&self) -> &'static str {
        match self {
            DurationChoice::FifteenMinutes => "Expires in 15 minutes.",
            DurationChoice::OneHour => "Expires in 1 hour.",
            DurationChoice::FourHours => "Expires in 4 hours.",
            DurationChoice::ThisSession => "Active for this terminal session.",
            DurationChoice::Forever => "Persisted to your user allowlist.",
        }
    }
}

/// Render `rule.when` as a YAML snippet for the preview label. We
/// serialise just the `when:` block (not the whole rule) so the
/// preview reads as the user sees it in their allowlist, without
/// the noisy `id:` / `created_*:` autoderived fields.
pub fn render_rule_when_yaml(rule: &Rule) -> String {
    match serde_yaml_ng::to_string(&rule.when) {
        Ok(s) => format!("when:\n{}", indent_lines(&s, "  ")),
        Err(e) => format!("(yaml render failed: {e})"),
    }
}

fn indent_lines(s: &str, pad: &str) -> String {
    s.lines()
        .map(|l| format!("{pad}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "../tests/cards_rules.rs"]
mod tests;
