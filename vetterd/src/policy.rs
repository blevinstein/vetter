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

use vetter_core::known_hosts::KnownHostsStore;
use vetter_core::matcher::{decide, AllowlistStore, Decision};
use vetter_core::parsers::Effect;
use vetter_core::wire::WireDecision;
use vetter_core::ParsedCommand;

use crate::pending::PromptSummary;

/// What the policy says we should do with this request.
// `Eq` was dropped along with `PromptSummary`'s `Eq` impl: the
// embedded `ParsedCommand` carries a `serde_json::Value` and other
// non-`Eq` parser extras. Test sites that need full equality use
// `PartialEq` (which is intentionally `f64`-aware) instead.
//
// The `PromptSummary` is `Box`ed because it now carries an
// optional full `ParsedCommand`, which is several hundred bytes
// on its own; without the indirection clippy correctly flags the
// enum as having a >400 byte variant alongside the small `Auto`
// variant. Boxing keeps the common (allow / deny) path cheap.
#[derive(Debug, Clone, PartialEq)]
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
    Prompt(Box<PromptSummary>),
}

/// Run the matcher against `parsed` and decide whether the daemon
/// can answer without a human or has to escalate.
///
/// `known_hosts` is consulted only on the prompt path, where it
/// populates the per-effect `host_known` hints inside the
/// resulting [`PromptSummary`] so the popover can paint
/// trust-coloured host pills. The matcher itself doesn't read it —
/// known/unknown is a UI signal, not an allow/deny rule.
pub fn evaluate(
    parsed: &ParsedCommand,
    request_id: &str,
    force_prompt: bool,
    store: &AllowlistStore,
    known_hosts: &KnownHostsStore,
) -> PolicyOutcome {
    if force_prompt {
        return PolicyOutcome::Prompt(Box::new(prompt_summary(
            request_id,
            parsed,
            true,
            known_hosts,
        )));
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
        Decision::Prompt => PolicyOutcome::Prompt(Box::new(prompt_summary(
            request_id,
            parsed,
            false,
            known_hosts,
        ))),
    }
}

fn prompt_summary(
    id: &str,
    parsed: &ParsedCommand,
    force_prompt: bool,
    known_hosts: &KnownHostsStore,
) -> PromptSummary {
    // Per-effect host-trust hints. `host_known[i]` answers "is
    // `effects[i]` an HttpRequest whose host the user has
    // explicitly recognised?" — used by the URL row to pick
    // between a green and an orange host pill. Loopback hosts
    // count as trusted by the same rule the analyzer uses to skip
    // them in `check_known_hosts`.
    let host_known: Vec<bool> = parsed
        .effects
        .iter()
        .map(|eff| match eff {
            Effect::HttpRequest(req) => is_known_host(req, known_hosts),
            _ => false,
        })
        .collect();
    PromptSummary {
        id: id.to_string(),
        command: parsed.command.clone(),
        primary_verb: parsed.display_hints.primary_verb.clone(),
        primary_target: parsed.display_hints.primary_target.clone(),
        force_prompt,
        // Carry the full RiskSignal records so pill tooltips can
        // surface the analyzer's `detail` string verbatim.
        signals: parsed.signals.clone(),
        parsed: Some(parsed.clone()),
        host_known,
    }
}

fn is_known_host(req: &vetter_core::parsers::HttpRequest, store: &KnownHostsStore) -> bool {
    use std::net::IpAddr;
    use std::str::FromStr;

    let Some(host) = req.url.host_str() else {
        return false;
    };
    // Loopback rule mirrors `signals::is_loopback_host` so the
    // popover and the analyzer agree on what counts as "trusted
    // local". For IP literals, parse and check `is_loopback`; for
    // bracketed IPv6 (`[::1]`) we trim the brackets first.
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = IpAddr::from_str(stripped) {
        if ip.is_loopback() {
            return true;
        }
    }
    store.contains(host)
}

#[cfg(test)]
#[path = "tests/policy.rs"]
mod tests;
