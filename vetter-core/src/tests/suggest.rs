//! Unit tests for [`crate::suggest`].
//!
//! Coverage targets the tier table and host-edge cases called out in
//! the module docs. Anything that touches glob matching is anchored
//! by an assertion against [`crate::matcher::glob`] so the suggested
//! rule actually fires for the originating request — guards against
//! the engine emitting a syntactically valid but semantically empty
//! pattern.

use url::Url;

use super::*;
use crate::known_hosts::{KnownHostEntry, KnownHostsStore};
use crate::matcher::decide::{decide, Decision};
use crate::matcher::loader::AllowlistStore;
use crate::matcher::rule::Rule;
use crate::{Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy};

fn http_request(method: HttpMethod, url: &str) -> HttpRequest {
    HttpRequest {
        method,
        url: Url::parse(url).expect("test URL parses"),
        headers: vec![],
        body: Body::None,
        auth: None,
        tls: TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    }
}

fn parsed_with_http(req: HttpRequest) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(req)],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

fn store_with(rules: Vec<Rule>) -> AllowlistStore {
    AllowlistStore {
        denylist: vec![],
        session: vec![],
        project: vec![],
        user: rules,
        builtin: vec![],
    }
}

fn assert_rule_covers(rule: &Rule, parsed: &ParsedCommand) {
    let store = store_with(vec![rule.clone()]);
    let dec = decide(parsed, &store);
    assert!(
        matches!(dec, Decision::Allow { .. }),
        "rule {rule:?} should cover {parsed:?} but decide() returned {dec:?}"
    );
}

// ── Allowlist suggestions ────────────────────────────────────────

#[test]
fn allowlist_emits_three_tiers_for_multi_segment_path() {
    let parsed = parsed_with_http(http_request(
        HttpMethod::Get,
        "https://api.github.com/repos/foo/bar",
    ));
    let suggestions = allowlist_suggestions(&parsed);
    assert_eq!(suggestions.len(), 3);
    assert_eq!(suggestions[0].tier, SuggestionTier::Exact);
    assert_eq!(suggestions[1].tier, SuggestionTier::PathGlob);
    assert_eq!(suggestions[2].tier, SuggestionTier::MethodHost);

    // Path-glob should replace only the trailing segment.
    let glob_path = suggestions[1]
        .rule
        .when
        .http
        .as_ref()
        .unwrap()
        .url
        .as_ref()
        .unwrap()
        .path
        .as_deref()
        .unwrap();
    assert_eq!(glob_path, "/repos/foo/*");
    let exact_path = suggestions[0]
        .rule
        .when
        .http
        .as_ref()
        .unwrap()
        .url
        .as_ref()
        .unwrap()
        .path
        .as_deref()
        .unwrap();
    assert_eq!(exact_path, "/repos/foo/bar");
    let host_path = suggestions[2]
        .rule
        .when
        .http
        .as_ref()
        .unwrap()
        .url
        .as_ref()
        .unwrap()
        .path
        .as_deref()
        .unwrap();
    assert_eq!(host_path, "/**");
}

#[test]
fn allowlist_each_tier_actually_covers_request() {
    let parsed = parsed_with_http(http_request(
        HttpMethod::Get,
        "https://api.github.com/repos/foo/bar",
    ));
    for s in allowlist_suggestions(&parsed) {
        assert_rule_covers(&s.rule, &parsed);
    }
}

#[test]
fn allowlist_skips_path_glob_for_single_segment() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://example.com/healthz"));
    let suggestions = allowlist_suggestions(&parsed);
    assert_eq!(suggestions.len(), 2);
    let tiers: Vec<_> = suggestions.iter().map(|s| s.tier).collect();
    assert_eq!(
        tiers,
        vec![SuggestionTier::Exact, SuggestionTier::MethodHost]
    );
}

#[test]
fn allowlist_skips_path_glob_for_root() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://example.com/"));
    let suggestions = allowlist_suggestions(&parsed);
    assert_eq!(suggestions.len(), 2);
    let tiers: Vec<_> = suggestions.iter().map(|s| s.tier).collect();
    assert_eq!(
        tiers,
        vec![SuggestionTier::Exact, SuggestionTier::MethodHost]
    );
}

#[test]
fn allowlist_returns_empty_when_no_http_effect() {
    let parsed = ParsedCommand {
        command: "noop".into(),
        argv: vec!["noop".into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    assert!(allowlist_suggestions(&parsed).is_empty());
}

#[test]
fn allowlist_id_is_stable_for_same_input() {
    let parsed = parsed_with_http(http_request(
        HttpMethod::Post,
        "https://api.example.com/v1/things/42",
    ));
    let a = allowlist_suggestions(&parsed);
    let b = allowlist_suggestions(&parsed);
    assert_eq!(
        a.iter().map(|s| s.rule.id.clone()).collect::<Vec<_>>(),
        b.iter().map(|s| s.rule.id.clone()).collect::<Vec<_>>(),
    );
    // Auto ids start with the canonical prefix from
    // `matcher::derive_auto_id`.
    for s in &a {
        assert!(s.rule.id.starts_with("auto-"), "{}", s.rule.id);
    }
}

#[test]
fn allowlist_method_is_preserved_per_tier() {
    let parsed = parsed_with_http(http_request(
        HttpMethod::Delete,
        "https://api.example.com/v1/things/42",
    ));
    for s in allowlist_suggestions(&parsed) {
        let methods = s.rule.when.http.as_ref().unwrap().method.clone().unwrap();
        assert_eq!(methods, vec![HttpMethod::Delete]);
    }
}

// ── Known-host suggestions ───────────────────────────────────────

#[test]
fn host_emits_exact_and_wildcard_for_subdomain() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://api.github.com/"));
    let store = KnownHostsStore::default();
    let s = host_suggestions(&parsed, &store);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].tier, HostTier::Exact);
    assert_eq!(s[0].entry.pattern, "api.github.com");
    assert_eq!(s[1].tier, HostTier::Wildcard);
    assert_eq!(s[1].entry.pattern, "*.github.com");
}

#[test]
fn host_skips_wildcard_for_apex() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://github.com/"));
    let store = KnownHostsStore::default();
    let s = host_suggestions(&parsed, &store);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].tier, HostTier::Exact);
    assert_eq!(s[0].entry.pattern, "github.com");
}

#[test]
fn host_skips_wildcard_for_www() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://www.example.com/"));
    let store = KnownHostsStore::default();
    let s = host_suggestions(&parsed, &store);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].entry.pattern, "www.example.com");
}

#[test]
fn host_skips_wildcard_for_ip_literal() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://203.0.113.42/"));
    let store = KnownHostsStore::default();
    let s = host_suggestions(&parsed, &store);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].entry.pattern, "203.0.113.42");
}

#[test]
fn host_returns_empty_for_loopback() {
    for url in [
        "http://localhost:8080/",
        "http://127.0.0.1/",
        "http://127.0.0.5/",
    ] {
        let parsed = parsed_with_http(http_request(HttpMethod::Get, url));
        let store = KnownHostsStore::default();
        assert!(host_suggestions(&parsed, &store).is_empty(), "url={url}");
    }
}

#[test]
fn host_returns_empty_when_already_known() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://api.github.com/"));
    let store = KnownHostsStore {
        builtin: vec![],
        user: vec![KnownHostEntry {
            pattern: "api.github.com".into(),
            note: None,
        }],
        project: vec![],
    };
    assert!(host_suggestions(&parsed, &store).is_empty());
}

#[test]
fn host_returns_empty_when_wildcard_already_covers() {
    let parsed = parsed_with_http(http_request(HttpMethod::Get, "https://api.github.com/"));
    let store = KnownHostsStore {
        builtin: vec![KnownHostEntry {
            pattern: "*.github.com".into(),
            note: None,
        }],
        user: vec![],
        project: vec![],
    };
    // Builtin covers the host already → both tiers redundant.
    assert!(host_suggestions(&parsed, &store).is_empty());
}

#[test]
fn host_returns_empty_when_no_http_effect() {
    let parsed = ParsedCommand {
        command: "noop".into(),
        argv: vec!["noop".into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    let store = KnownHostsStore::default();
    assert!(host_suggestions(&parsed, &store).is_empty());
}
