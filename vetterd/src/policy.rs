//! Wraps [`vetter_core::matcher::decide`] with the Phase-3 stub UI.
//!
//! The matcher returns `Allow{rule_id, scope}` / `Deny{rule_id,
//! scope}` / `Prompt`. We project those onto the wire's
//! [`WireDecision`] enum and add a human-readable `reason` string.
//! `Prompt` (or any `force_prompt: true` request, i.e. `vet
//! --dry-run`) gets the stub treatment: there is no human UI yet, so
//! we auto-deny with `"no UI yet (Phase 4)"`. The CLI surfaces that
//! verbatim so users understand why the request was refused.
//!
//! Note: `evaluate` takes a [`vetter_core::ParsedCommand`] *and* the
//! `force_prompt` flag separately — the daemon owns the parse step
//! (see `plans/ThreatModel.md` T2 / wire v2) and only the
//! out-of-band `force_prompt` toggle still rides the wire.

use vetter_core::matcher::{decide, AllowlistStore, Decision};
use vetter_core::wire::WireDecision;
use vetter_core::ParsedCommand;

/// Human-readable reason for the `force_prompt` / no-rule-match
/// branch. Single source of truth so tests can assert on the exact
/// string the daemon emits.
pub const STUB_PROMPT_REASON: &str = "no UI yet (Phase 4)";

/// Run the matcher against `parsed`, project the matcher's decision
/// onto a wire decision, and return a `(decision, reason)` pair ready
/// to embed in a [`vetter_core::wire::VetDecision`].
pub fn evaluate(
    parsed: &ParsedCommand,
    force_prompt: bool,
    store: &AllowlistStore,
) -> (WireDecision, String) {
    if force_prompt {
        return (WireDecision::Deny, STUB_PROMPT_REASON.to_string());
    }
    match decide(parsed, store) {
        Decision::Allow { rule_id, scope } => (
            WireDecision::Allow,
            format!("matched rule `{rule_id}` in {}", scope.as_str()),
        ),
        Decision::Deny { rule_id, scope } => (
            WireDecision::Deny,
            format!("denylist rule `{rule_id}` in {}", scope.as_str()),
        ),
        Decision::Prompt => (WireDecision::Deny, STUB_PROMPT_REASON.to_string()),
    }
}

#[cfg(test)]
#[path = "tests/policy.rs"]
mod tests;
