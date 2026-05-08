//! Property-based tests for the rule matcher.
//!
//! Strategy: generate a focused universe of `(ParsedCommand, Rule)`
//! pairs that we know match, then assert:
//!   1. matches_rule(parsed, rule) == true (sanity).
//!   2. Mutating any single field of the request flips the result to
//!      false.
//!   3. matches_rule never panics on any well-formed input within the
//!      generated universe.

use proptest::prelude::*;
use url::Url;
use vetter_core::matcher::{matches_rule, HostPattern, HttpClause, Rule, RuleWhen, UrlClause};
use vetter_core::{
    Body, DisplayHints, Effect, Header, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
};

#[derive(Debug, Clone)]
struct Pair {
    parsed: ParsedCommand,
    rule: Rule,
    method: HttpMethod,
    host: String,
    path: String,
    headers: Vec<(String, String)>,
}

fn build_request(
    method: HttpMethod,
    host: &str,
    path: &str,
    headers: &[(String, String)],
    inline_body: bool,
) -> HttpRequest {
    build_request_full(method, host, path, headers, inline_body, false)
}

fn build_request_full(
    method: HttpMethod,
    host: &str,
    path: &str,
    headers: &[(String, String)],
    inline_body: bool,
    follow_redirects: bool,
) -> HttpRequest {
    let url_str = format!("https://{host}{path}");
    let url = Url::parse(&url_str).expect("test url");
    HttpRequest {
        method,
        url,
        headers: headers
            .iter()
            .map(|(k, v)| Header {
                name: k.clone(),
                value: v.clone(),
            })
            .collect(),
        body: if inline_body {
            Body::Inline { bytes: vec![1] }
        } else {
            Body::None
        },
        auth: None,
        tls: TlsPolicy::Strict,
        follow_redirects,
        proxy: None,
    }
}

fn build_parsed(req: HttpRequest) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into()],
        cwd: None,
        effects: vec![Effect::HttpRequest(req)],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

/// Build a parsed/rule pair where the rule **strictly** requires the
/// request shape: exact method, exact host, exact path, exact set of
/// allowed headers, and `no_body: true`. Mutating any of those fields
/// should flip the result.
fn build_pair(
    method: HttpMethod,
    host: String,
    path: String,
    headers: Vec<(String, String)>,
) -> Pair {
    let req = build_request(method.clone(), &host, &path, &headers, false);
    let parsed = build_parsed(req);
    let header_allow: Vec<String> = headers.iter().map(|(k, _)| k.clone()).collect();
    let rule = Rule {
        id: "p".into(),
        command: None,
        when: RuleWhen {
            http: Some(HttpClause {
                method: Some(vec![method.clone()]),
                url: Some(UrlClause {
                    scheme: Some("https".into()),
                    host: Some(HostPattern::One(host.clone())),
                    port: Some(vec![443]),
                    path: Some(path.clone()),
                }),
                headers_allow: Some(header_allow),
                no_body: Some(true),
                query: None,
                no_redirects: Some(false),
            }),
            file_write: None,
            file_read: None,
        },
        note: None,
        created_by: None,
        created_at: None,
    };
    Pair {
        parsed,
        rule,
        method,
        host,
        path,
        headers,
    }
}

fn arb_method() -> impl Strategy<Value = HttpMethod> {
    prop_oneof![
        Just(HttpMethod::Get),
        Just(HttpMethod::Post),
        Just(HttpMethod::Put),
        Just(HttpMethod::Patch),
        Just(HttpMethod::Delete),
        Just(HttpMethod::Head),
    ]
}

fn arb_host() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("api.example.test".to_string()),
        Just("example.test".to_string()),
        Just("svc.internal.test".to_string()),
    ]
}

fn arb_path() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("/v1/foo".to_string()),
        Just("/v1/foo/bar".to_string()),
        Just("/repos/abc".to_string()),
        Just("/".to_string()),
    ]
}

fn arb_headers() -> impl Strategy<Value = Vec<(String, String)>> {
    prop_oneof![
        Just(Vec::<(String, String)>::new()),
        Just(vec![("Accept".to_string(), "*/*".to_string())]),
        Just(vec![
            ("Accept".to_string(), "application/json".to_string()),
            ("User-Agent".to_string(), "vet-test".to_string()),
        ]),
    ]
}

fn arb_pair() -> impl Strategy<Value = Pair> {
    (arb_method(), arb_host(), arb_path(), arb_headers())
        .prop_map(|(m, h, p, hdrs)| build_pair(m, h, p, hdrs))
}

proptest! {
    #[test]
    fn matches_rule_holds_for_constructed_pair(pair in arb_pair()) {
        prop_assert!(
            matches_rule(&pair.parsed, &pair.rule),
            "expected rule to match its own parsed command: {pair:?}",
        );
    }

    #[test]
    fn mutating_method_flips_to_no_match(pair in arb_pair()) {
        let other_method = if pair.method == HttpMethod::Get {
            HttpMethod::Patch
        } else {
            HttpMethod::Get
        };
        let req = build_request(other_method, &pair.host, &pair.path, &pair.headers, false);
        let parsed = build_parsed(req);
        prop_assert!(!matches_rule(&parsed, &pair.rule));
    }

    #[test]
    fn mutating_host_flips_to_no_match(pair in arb_pair()) {
        let other_host = if pair.host == "altered.test" {
            "rotated.test"
        } else {
            "altered.test"
        };
        let req = build_request(pair.method.clone(), other_host, &pair.path, &pair.headers, false);
        let parsed = build_parsed(req);
        prop_assert!(!matches_rule(&parsed, &pair.rule));
    }

    #[test]
    fn mutating_path_flips_to_no_match(pair in arb_pair()) {
        let other_path = if pair.path == "/__elsewhere" {
            "/__different"
        } else {
            "/__elsewhere"
        };
        let req = build_request(pair.method.clone(), &pair.host, other_path, &pair.headers, false);
        let parsed = build_parsed(req);
        prop_assert!(!matches_rule(&parsed, &pair.rule));
    }

    #[test]
    fn adding_unlisted_header_flips_to_no_match(pair in arb_pair()) {
        let mut headers = pair.headers.clone();
        headers.push(("X-Forbidden-Header".to_string(), "x".to_string()));
        let req = build_request(pair.method.clone(), &pair.host, &pair.path, &headers, false);
        let parsed = build_parsed(req);
        prop_assert!(!matches_rule(&parsed, &pair.rule));
    }

    #[test]
    fn adding_body_flips_to_no_match(pair in arb_pair()) {
        let req = build_request(pair.method.clone(), &pair.host, &pair.path, &pair.headers, true);
        let parsed = build_parsed(req);
        prop_assert!(!matches_rule(&parsed, &pair.rule));
    }

    #[test]
    fn matches_rule_never_panics(pair in arb_pair()) {
        let _ = matches_rule(&pair.parsed, &pair.rule);
    }

    /// Closes ThreatModel T10. With `no_redirects: Some(false)` (the
    /// `arb_pair` builder's choice — it explicitly opts into
    /// redirect-following trust so the existing match invariants
    /// hold uniformly), the rule must accept a request regardless of
    /// the `follow_redirects` axis.
    #[test]
    fn no_redirects_false_admits_any_follow_redirects(pair in arb_pair(), follow in any::<bool>()) {
        let req = build_request_full(
            pair.method.clone(),
            &pair.host,
            &pair.path,
            &pair.headers,
            false,
            follow,
        );
        let parsed = build_parsed(req);
        prop_assert!(
            matches_rule(&parsed, &pair.rule),
            "no_redirects=false rule should not reject on follow_redirects={follow}: {pair:?}",
        );
    }

    /// Default-deny: stripping the explicit `no_redirects: false`
    /// opt-in must cause the rule to reject any request with
    /// `follow_redirects = true`, regardless of every other axis.
    #[test]
    fn no_redirects_omitted_blocks_follow_redirects_request(pair in arb_pair()) {
        let req = build_request_full(
            pair.method.clone(),
            &pair.host,
            &pair.path,
            &pair.headers,
            false,
            true,
        );
        let parsed = build_parsed(req);
        let mut rule = pair.rule.clone();
        if let Some(http) = rule.when.http.as_mut() {
            http.no_redirects = None;
        }
        prop_assert!(
            !matches_rule(&parsed, &rule),
            "no_redirects=None rule should reject follow_redirects=true: {pair:?}",
        );
    }
}
