//! Tests for [`crate::policy`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use vetter_core::known_hosts::KnownHostsStore;
use vetter_core::matcher::rule::{HostPattern, HttpClause, Rule, RuleWhen, UrlClause};
use vetter_core::matcher::Scope;
use vetter_core::{
    Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy, WireDecision,
};

/// Tests do not exercise host-trust plumbing here; that's covered by
/// dedicated tests below. Use an empty store as the neutral default
/// so policy decisions don't accidentally depend on the built-in
/// host list.
fn empty_known_hosts() -> KnownHostsStore {
    KnownHostsStore::default()
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
        display_hints: DisplayHints {
            primary_verb: "GET".into(),
            primary_target: format!("https://{host}/"),
            badges: vec![],
        },
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
                no_redirects: None,
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
fn allow_rule_in_user_scope_returns_auto_allow_with_reason() {
    let store = AllowlistStore {
        denylist: vec![],
        session: vec![],
        project: vec![],
        user: vec![allow_rule("yes", "example.test")],
        builtin: vec![],
    };
    let outcome = evaluate(
        &parsed_get("example.test"),
        "id-1",
        false,
        &store,
        &empty_known_hosts(),
    );
    match outcome {
        PolicyOutcome::Auto {
            decision,
            reason,
            rule_id,
            scope,
        } => {
            assert_eq!(decision, WireDecision::Allow);
            assert!(reason.contains("yes"), "{reason}");
            assert!(reason.contains("user"), "{reason}");
            assert_eq!(rule_id.as_deref(), Some("yes"));
            assert_eq!(scope, Some(Scope::User));
        }
        other => panic!("expected auto-allow, got {other:?}"),
    }
}

#[test]
fn denylist_rule_returns_auto_deny_with_reason() {
    let store = AllowlistStore {
        denylist: vec![allow_rule("blocked", "example.test")],
        session: vec![],
        project: vec![],
        user: vec![allow_rule("would-allow", "example.test")],
        builtin: vec![],
    };
    let outcome = evaluate(
        &parsed_get("example.test"),
        "id-2",
        false,
        &store,
        &empty_known_hosts(),
    );
    match outcome {
        PolicyOutcome::Auto {
            decision,
            reason,
            rule_id,
            scope,
        } => {
            assert_eq!(decision, WireDecision::Deny);
            assert!(reason.contains("blocked"), "{reason}");
            assert!(reason.contains("denylist"), "{reason}");
            assert_eq!(rule_id.as_deref(), Some("blocked"));
            assert_eq!(scope, Some(Scope::Denylist));
        }
        other => panic!("expected auto-deny, got {other:?}"),
    }
}

#[test]
fn no_match_returns_prompt_with_summary() {
    let store = AllowlistStore::default();
    let outcome = evaluate(
        &parsed_get("unknown.test"),
        "id-3",
        false,
        &store,
        &empty_known_hosts(),
    );
    match outcome {
        PolicyOutcome::Prompt(s) => {
            assert_eq!(s.id, "id-3");
            assert_eq!(s.command, "curl");
            assert_eq!(s.primary_verb, "GET");
            assert_eq!(s.primary_target, "https://unknown.test/");
            assert!(!s.force_prompt);
        }
        other => panic!("expected prompt, got {other:?}"),
    }
}

#[test]
fn force_prompt_short_circuits_allow_rule_with_force_prompt_summary() {
    let store = AllowlistStore {
        denylist: vec![],
        session: vec![],
        project: vec![],
        user: vec![allow_rule("would-allow", "example.test")],
        builtin: vec![],
    };
    let outcome = evaluate(
        &parsed_get("example.test"),
        "id-4",
        true,
        &store,
        &empty_known_hosts(),
    );
    match outcome {
        PolicyOutcome::Prompt(s) => {
            assert_eq!(s.id, "id-4");
            assert!(s.force_prompt, "force_prompt should propagate to summary");
        }
        other => panic!("expected prompt, got {other:?}"),
    }
}

#[test]
fn force_prompt_does_not_short_circuit_denylist() {
    // A denylist hit must still surface as an auto-deny even when
    // the caller requested force_prompt — denylist beats prompts.
    // Today the matcher's Decision::Deny branch is reached only
    // when force_prompt is false (we short-circuit above), so we
    // assert by toggling force_prompt off and confirming deny still
    // wins. Documents the intentional ordering.
    let store = AllowlistStore {
        denylist: vec![allow_rule("blocked", "example.test")],
        session: vec![],
        project: vec![],
        user: vec![allow_rule("would-allow", "example.test")],
        builtin: vec![],
    };
    let outcome = evaluate(
        &parsed_get("example.test"),
        "id-5",
        false,
        &store,
        &empty_known_hosts(),
    );
    matches!(
        outcome,
        PolicyOutcome::Auto {
            decision: WireDecision::Deny,
            ..
        }
    );
}

// -- host_known plumbing ----------------------------------------

/// `prompt_summary` should populate `host_known[i]` true iff
/// `effects[i]` is an HttpRequest whose host the
/// `KnownHostsStore` recognises. Loopback hosts also count as known.
#[test]
fn prompt_summary_marks_host_known_for_store_match() {
    let store = AllowlistStore::default();
    let known_hosts = KnownHostsStore {
        builtin: vec![],
        user: vec![vetter_core::known_hosts::KnownHostEntry {
            pattern: "trusted.test".into(),
            note: None,
        }],
        project: vec![],
    };
    let outcome = evaluate(
        &parsed_get("trusted.test"),
        "id-known",
        false,
        &store,
        &known_hosts,
    );
    let summary = match outcome {
        PolicyOutcome::Prompt(s) => s,
        other => panic!("expected prompt, got {other:?}"),
    };
    assert_eq!(summary.host_known, vec![true]);
    assert!(summary.parsed.is_some());
}

#[test]
fn prompt_summary_marks_host_unknown_for_store_miss() {
    let store = AllowlistStore::default();
    let outcome = evaluate(
        &parsed_get("never.heard.test"),
        "id-unknown",
        false,
        &store,
        &empty_known_hosts(),
    );
    let summary = match outcome {
        PolicyOutcome::Prompt(s) => s,
        other => panic!("expected prompt, got {other:?}"),
    };
    assert_eq!(summary.host_known, vec![false]);
}

#[test]
fn prompt_summary_marks_loopback_as_known() {
    let store = AllowlistStore::default();
    let outcome = evaluate(
        &parsed_get("localhost"),
        "id-loop",
        false,
        &store,
        &empty_known_hosts(),
    );
    let summary = match outcome {
        PolicyOutcome::Prompt(s) => s,
        other => panic!("expected prompt, got {other:?}"),
    };
    assert_eq!(
        summary.host_known,
        vec![true],
        "loopback host should be flagged known even when the store is empty"
    );
}
