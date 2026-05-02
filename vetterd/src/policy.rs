//! Wraps [`vetter_core::matcher::decide`] with the Phase-3a stub UI.
//!
//! The matcher returns `Allow{rule_id, scope}` / `Deny{rule_id,
//! scope}` / `Prompt`. We project those onto the wire's
//! [`WireDecision`] enum and add a human-readable `reason` string.
//! `Prompt` (or any `force_prompt: true` request, i.e. `vet
//! --dry-run`) gets the stub treatment: there is no human UI yet, so
//! we auto-deny with `"no UI yet (Phase 4)"`. The CLI surfaces that
//! verbatim so users understand why the request was refused.

use vetter_core::matcher::{decide, AllowlistStore, Decision};
use vetter_core::wire::{VetRequest, WireDecision};

/// Human-readable reason for the `force_prompt` / no-rule-match
/// branch. Single source of truth so tests can assert on the exact
/// string the daemon emits.
pub const STUB_PROMPT_REASON: &str = "no UI yet (Phase 4)";

/// Run the matcher, project the matcher's decision onto a wire
/// decision, and return a `(decision, reason)` pair ready to embed
/// in a [`vetter_core::wire::VetDecision`].
pub fn evaluate(req: &VetRequest, store: &AllowlistStore) -> (WireDecision, String) {
    if req.force_prompt {
        return (WireDecision::Deny, STUB_PROMPT_REASON.to_string());
    }
    match decide(&req.parsed, store) {
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
mod tests {
    use super::*;
    use vetter_core::matcher::rule::{HostPattern, HttpClause, Rule, RuleWhen, UrlClause};
    use vetter_core::wire::PROTOCOL_VERSION;
    use vetter_core::{
        Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };

    fn req_for(parsed: ParsedCommand, force_prompt: bool) -> VetRequest {
        VetRequest {
            v: PROTOCOL_VERSION,
            id: "01HX0000000000000000000000".into(),
            cwd: None,
            agent_hint: None,
            command: parsed.command.clone(),
            argv: parsed.argv.clone(),
            stdin_digest: None,
            parsed,
            force_prompt,
        }
    }

    fn parsed_get(host: &str) -> ParsedCommand {
        ParsedCommand {
            command: "curl".into(),
            argv: vec!["curl".into(), format!("https://{host}/")],
            cwd: None,
            stdin_digest: None,
            effects: vec![Effect::HttpRequest(HttpRequest {
                method: HttpMethod::Get,
                url: url::Url::parse(&format!("https://{host}/")).unwrap(),
                headers: vec![],
                body: Body::None,
                auth: None,
                tls: TlsPolicy::Strict,
                follow_redirects: false,
                proxy: None,
            })],
            signals: vec![],
            display_hints: DisplayHints::default(),
            extras: serde_json::Value::Null,
        }
    }

    fn allow_rule(id: &str, host: &str) -> Rule {
        Rule {
            id: id.into(),
            command: None,
            when: RuleWhen {
                http: Some(HttpClause {
                    method: Some(vec![HttpMethod::Get]),
                    url: Some(UrlClause {
                        scheme: Some("https".into()),
                        host: Some(HostPattern::One(host.into())),
                        port: None,
                        path: None,
                    }),
                    headers_allow: Some(vec!["*".into()]),
                    no_body: None,
                    query: None,
                }),
                file_write: None,
                file_read: None,
            },
            note: None,
            created_by: None,
            created_at: None,
        }
    }

    #[test]
    fn allow_rule_in_user_scope_returns_allow_with_reason() {
        let store = AllowlistStore {
            denylist: vec![],
            session: vec![],
            project: vec![],
            user: vec![allow_rule("yes", "example.test")],
            builtin: vec![],
        };
        let req = req_for(parsed_get("example.test"), false);
        let (d, reason) = evaluate(&req, &store);
        assert_eq!(d, WireDecision::Allow);
        assert!(reason.contains("yes"), "{reason}");
        assert!(reason.contains("user"), "{reason}");
    }

    #[test]
    fn denylist_rule_returns_deny_with_reason() {
        let store = AllowlistStore {
            denylist: vec![allow_rule("blocked", "example.test")],
            session: vec![],
            project: vec![],
            user: vec![allow_rule("would-allow", "example.test")],
            builtin: vec![],
        };
        let req = req_for(parsed_get("example.test"), false);
        let (d, reason) = evaluate(&req, &store);
        assert_eq!(d, WireDecision::Deny);
        assert!(reason.contains("blocked"), "{reason}");
        assert!(reason.contains("denylist"), "{reason}");
    }

    #[test]
    fn no_match_falls_back_to_stub_deny() {
        let store = AllowlistStore::default();
        let req = req_for(parsed_get("unknown.test"), false);
        let (d, reason) = evaluate(&req, &store);
        assert_eq!(d, WireDecision::Deny);
        assert_eq!(reason, STUB_PROMPT_REASON);
    }

    #[test]
    fn force_prompt_short_circuits_even_with_allow_rule() {
        let store = AllowlistStore {
            denylist: vec![],
            session: vec![],
            project: vec![],
            user: vec![allow_rule("would-allow", "example.test")],
            builtin: vec![],
        };
        let req = req_for(parsed_get("example.test"), true);
        let (d, reason) = evaluate(&req, &store);
        assert_eq!(d, WireDecision::Deny);
        assert_eq!(reason, STUB_PROMPT_REASON);
    }
}
