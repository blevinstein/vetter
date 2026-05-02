//! Wraps [`vetter_core::matcher::decide`] with the prompt-class
//! routing rules from `plans/Overview.md` §5.
//!
//! The matcher returns `Allow{rule_id, scope}` / `Deny{rule_id, scope}`
//! / `Prompt`. We project those onto a [`PolicyOutcome`]:
//!
//! - `Auto(WireDecision::Allow, ...)` for matched allow rules.
//! - `Auto(WireDecision::Deny, ...)` for matched denylist rules.
//! - `Prompt(...)` for everything else (no-match, or any
//!   `force_prompt: true` request from `vet --dry-run`).
//!
//! The Phase 3a stub-deny path is gone — prompt-class outcomes now
//! route through [`crate::pending::PendingQueue`] and the
//! [`crate::notifier::Notifier`] surface. See `plans/ThreatModel.md`
//! T2 for why the daemon owns the parse step the matcher consumes.

use vetter_core::matcher::{decide, AllowlistStore, Decision};
use vetter_core::wire::WireDecision;
use vetter_core::ParsedCommand;

use crate::pending::PromptSummary;

/// What the policy says we should do with this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyOutcome {
    /// Final decision the daemon can ship to the wire without
    /// asking the user.
    Auto {
        decision: WireDecision,
        reason: String,
    },
    /// Prompt-class request: the matcher found no automatic
    /// resolution (or the caller asked for `force_prompt`). The
    /// daemon should hand `summary` to the [`crate::notifier::Notifier`]
    /// and park the worker on the pending queue.
    Prompt(PromptSummary),
}

/// Run the matcher against `parsed` and decide whether the daemon
/// can answer without a human or has to escalate.
pub fn evaluate(
    parsed: &ParsedCommand,
    request_id: &str,
    force_prompt: bool,
    store: &AllowlistStore,
) -> PolicyOutcome {
    if force_prompt {
        return PolicyOutcome::Prompt(prompt_summary(request_id, parsed, true));
    }
    match decide(parsed, store) {
        Decision::Allow { rule_id, scope } => PolicyOutcome::Auto {
            decision: WireDecision::Allow,
            reason: format!("matched rule `{rule_id}` in {}", scope.as_str()),
        },
        Decision::Deny { rule_id, scope } => PolicyOutcome::Auto {
            decision: WireDecision::Deny,
            reason: format!("denylist rule `{rule_id}` in {}", scope.as_str()),
        },
        Decision::Prompt => PolicyOutcome::Prompt(prompt_summary(request_id, parsed, false)),
    }
}

fn prompt_summary(id: &str, parsed: &ParsedCommand, force_prompt: bool) -> PromptSummary {
    PromptSummary {
        id: id.to_string(),
        command: parsed.command.clone(),
        primary_verb: parsed.display_hints.primary_verb.clone(),
        primary_target: parsed.display_hints.primary_target.clone(),
        force_prompt,
        // Project signals down to their `kind` — the popover only
        // needs the kind for chip rendering / severity routing,
        // not the per-effect detail string (which is already in
        // the §8.5 body).
        signals: parsed.signals.iter().map(|s| s.kind).collect(),
    }
}

#[cfg(test)]
#[path = "tests/policy.rs"]
mod tests;
