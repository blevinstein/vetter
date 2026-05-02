//! Tests for [`crate::matcher::decide`]. Layout convention is
//! described in `AGENTS.md`.

use super::*;
use crate::matcher::loader::AllowlistStore;
use crate::matcher::rule::{HostPattern, RuleWhen, UrlClause};
use crate::{
    Auth, DisplayHints, FileRead, FileWrite, Header, HttpMethod, HttpRequest, ParsedCommand,
    Sha256, TlsPolicy, WriteSource,
};
use url::Url;

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn http(method: HttpMethod, u: &str) -> HttpRequest {
    HttpRequest {
        method,
        url: url(u),
        headers: vec![],
        body: Body::None,
        auth: None,
        tls: TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    }
}

fn parsed_with(effects: Vec<Effect>) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into()],
        cwd: None,
        stdin_digest: None,
        effects,
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

fn http_rule(id: &str, http: HttpClause) -> Rule {
    Rule {
        id: id.into(),
        command: None,
        when: RuleWhen {
            http: Some(http),
            file_write: None,
            file_read: None,
        },
        note: None,
        created_by: None,
        created_at: None,
    }
}

fn allow_clause() -> HttpClause {
    HttpClause {
        method: Some(vec![HttpMethod::Get]),
        url: Some(UrlClause {
            scheme: Some("https".into()),
            host: Some(HostPattern::One("example.test".into())),
            port: Some(vec![443]),
            path: Some("/v1/**".into()),
        }),
        headers_allow: Some(vec!["*".into()]),
        no_body: Some(true),
        query: None,
    }
}

#[test]
fn matches_method_and_url_match() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    assert!(matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn method_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Post,
        "https://example.test/v1/foo",
    ))]);
    assert!(!matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn scheme_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "http://example.test/v1/foo",
    ))]);
    assert!(!matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn host_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://other.test/v1/foo",
    ))]);
    assert!(!matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn port_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test:8443/v1/foo",
    ))]);
    assert!(!matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn path_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v2/foo",
    ))]);
    assert!(!matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn path_traversal_normalised_then_rejected() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/../v2/foo",
    ))]);
    assert!(!matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn no_body_blocks_inline_body() {
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.body = Body::Inline { bytes: vec![1] };
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    assert!(!matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn headers_allow_default_deny_blocks_any_header() {
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.headers = vec![Header {
        name: "X-Custom".into(),
        value: "1".into(),
    }];
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    let mut clause = allow_clause();
    clause.headers_allow = None;
    assert!(!matches_rule(&parsed, &http_rule("ok", clause)));
}

#[test]
fn headers_allow_explicit_list_admits_listed_only() {
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.headers = vec![Header {
        name: "Accept".into(),
        value: "*/*".into(),
    }];
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    let mut clause = allow_clause();
    clause.headers_allow = Some(vec!["Accept".into()]);
    assert!(matches_rule(&parsed, &http_rule("ok", clause.clone())));

    let mut req2 = http(HttpMethod::Get, "https://example.test/v1/foo");
    req2.headers = vec![Header {
        name: "X-Other".into(),
        value: "x".into(),
    }];
    let parsed2 = parsed_with(vec![Effect::HttpRequest(req2)]);
    assert!(!matches_rule(&parsed2, &http_rule("ok", clause)));
}

#[test]
fn headers_allow_wildcard_admits_anything() {
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.headers = vec![
        Header {
            name: "Accept".into(),
            value: "x".into(),
        },
        Header {
            name: "X-Custom".into(),
            value: "1".into(),
        },
    ];
    req.auth = Some(Auth::Bearer {
        token_redacted: true,
    });
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    assert!(matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn query_default_off_ignores_query() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo?x=1",
    ))]);
    assert!(matches_rule(&parsed, &http_rule("ok", allow_clause())));
}

#[test]
fn query_required_when_set_true() {
    let mut clause = allow_clause();
    clause.query = Some(true);
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    assert!(!matches_rule(&parsed, &http_rule("ok", clause.clone())));
    let parsed2 = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo?x=1",
    ))]);
    assert!(matches_rule(&parsed2, &http_rule("ok", clause)));
}

#[test]
fn command_filter_narrows_match() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let mut rule = http_rule("ok", allow_clause());
    rule.command = Some("curl".into());
    assert!(matches_rule(&parsed, &rule));
    rule.command = Some("noop".into());
    assert!(!matches_rule(&parsed, &rule));
}

#[test]
fn empty_when_does_not_match() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let rule = Rule {
        id: "blank".into(),
        command: None,
        when: RuleWhen::default(),
        note: None,
        created_by: None,
        created_at: None,
    };
    assert!(!matches_rule(&parsed, &rule));
}

#[test]
fn multi_effect_rule_requires_each_clause() {
    let parsed_one = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let multi = Rule {
        id: "multi".into(),
        command: None,
        when: RuleWhen {
            http: Some(allow_clause()),
            file_write: Some(FileWriteClause {
                path: Some("/tmp/**".into()),
            }),
            file_read: None,
        },
        note: None,
        created_by: None,
        created_at: None,
    };
    assert!(!matches_rule(&parsed_one, &multi));

    let parsed_both = parsed_with(vec![
        Effect::HttpRequest(http(HttpMethod::Get, "https://example.test/v1/foo")),
        Effect::FileWrite(FileWrite {
            path: "/tmp/out".into(),
            source: WriteSource::RemoteHttp {
                url: url("https://example.test/v1/foo"),
            },
            overwrite: false,
        }),
    ]);
    assert!(matches_rule(&parsed_both, &multi));
}

#[test]
fn file_read_clause_normalises_traversal() {
    let parsed = parsed_with(vec![Effect::FileRead(FileRead {
        path: "/srv/../etc/passwd".into(),
    })]);
    let rule = Rule {
        id: "fr".into(),
        command: None,
        when: RuleWhen {
            http: None,
            file_write: None,
            file_read: Some(FileReadClause {
                path: Some("/srv/**".into()),
            }),
        },
        note: None,
        created_by: None,
        created_at: None,
    };
    assert!(!matches_rule(&parsed, &rule));
}

#[test]
fn layered_precedence_denylist_first() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let allow = http_rule("allow-it", allow_clause());
    let deny = http_rule("block-it", allow_clause());
    let store = AllowlistStore {
        denylist: vec![deny.clone()],
        session: vec![],
        project: vec![allow.clone()],
        user: vec![],
        builtin: vec![],
    };
    match decide(&parsed, &store) {
        Decision::Deny { rule_id, scope } => {
            assert_eq!(rule_id, "block-it");
            assert_eq!(scope, Scope::Denylist);
        }
        other => panic!("expected deny, got {other:?}"),
    }
}

#[test]
fn layered_precedence_session_beats_project() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let store = AllowlistStore {
        denylist: vec![],
        session: vec![http_rule("from-session", allow_clause())],
        project: vec![http_rule("from-project", allow_clause())],
        user: vec![http_rule("from-user", allow_clause())],
        builtin: vec![http_rule("from-builtin", allow_clause())],
    };
    match decide(&parsed, &store) {
        Decision::Allow { rule_id, scope } => {
            assert_eq!(rule_id, "from-session");
            assert_eq!(scope, Scope::Session);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn layered_precedence_project_beats_user_beats_builtin() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let store = AllowlistStore {
        denylist: vec![],
        session: vec![],
        project: vec![http_rule("from-project", allow_clause())],
        user: vec![http_rule("from-user", allow_clause())],
        builtin: vec![http_rule("from-builtin", allow_clause())],
    };
    let d = decide(&parsed, &store);
    assert!(
        matches!(&d, Decision::Allow { rule_id, scope: Scope::Project } if rule_id == "from-project"),
        "{d:?}",
    );
    let store_no_project = AllowlistStore {
        denylist: vec![],
        session: vec![],
        project: vec![],
        user: vec![http_rule("from-user", allow_clause())],
        builtin: vec![http_rule("from-builtin", allow_clause())],
    };
    let d = decide(&parsed, &store_no_project);
    assert!(
        matches!(&d, Decision::Allow { rule_id, scope: Scope::User } if rule_id == "from-user"),
        "{d:?}",
    );
    let store_only_builtin = AllowlistStore {
        denylist: vec![],
        session: vec![],
        project: vec![],
        user: vec![],
        builtin: vec![http_rule("from-builtin", allow_clause())],
    };
    let d = decide(&parsed, &store_only_builtin);
    assert!(
        matches!(&d, Decision::Allow { rule_id, scope: Scope::Builtin } if rule_id == "from-builtin"),
        "{d:?}",
    );
}

#[test]
fn no_match_returns_prompt() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Post,
        "https://example.test/v1/foo",
    ))]);
    let store = AllowlistStore {
        denylist: vec![],
        session: vec![],
        project: vec![http_rule("ok", allow_clause())],
        user: vec![],
        builtin: vec![],
    };
    assert_eq!(decide(&parsed, &store), Decision::Prompt);
}

// exhaust unused-import warnings while keeping the imports that
// tests actually use elsewhere
#[allow(dead_code)]
fn _ensure_sha256(_: Sha256) {}
