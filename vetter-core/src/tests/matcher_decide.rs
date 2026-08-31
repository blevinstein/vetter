//! Tests for [`crate::matcher::decide`]. Layout convention is
//! described in `AGENTS.md`.

use super::*;
use crate::matcher::loader::AllowlistStore;
use crate::matcher::rule::{HostPattern, RuleWhen, UrlClause};
use crate::{
    Auth, DisplayHints, FileRead, FileWrite, Header, HttpMethod, HttpRequest, ParsedCommand,
    TlsPolicy, WriteSource,
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
        expires_at: None,
        sid: None,
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
        no_redirects: None,
    }
}

#[test]
fn matches_method_and_url_match() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    assert!(matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn method_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Post,
        "https://example.test/v1/foo",
    ))]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn scheme_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "http://example.test/v1/foo",
    ))]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn host_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://other.test/v1/foo",
    ))]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn port_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test:8443/v1/foo",
    ))]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn path_mismatch_rejects() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v2/foo",
    ))]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn path_traversal_normalised_then_rejected() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/../v2/foo",
    ))]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn no_body_blocks_inline_body() {
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.body = Body::Inline { bytes: vec![1] };
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
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
    assert!(!matches_rule(&parsed, &http_rule("ok", clause), 0, None));
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
    assert!(matches_rule(
        &parsed,
        &http_rule("ok", clause.clone()),
        0,
        None
    ));

    let mut req2 = http(HttpMethod::Get, "https://example.test/v1/foo");
    req2.headers = vec![Header {
        name: "X-Other".into(),
        value: "x".into(),
    }];
    let parsed2 = parsed_with(vec![Effect::HttpRequest(req2)]);
    assert!(!matches_rule(&parsed2, &http_rule("ok", clause), 0, None));
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
    assert!(matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn query_default_off_ignores_query() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo?x=1",
    ))]);
    assert!(matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn query_required_when_set_true() {
    let mut clause = allow_clause();
    clause.query = Some(true);
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", clause.clone()),
        0,
        None
    ));
    let parsed2 = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo?x=1",
    ))]);
    assert!(matches_rule(&parsed2, &http_rule("ok", clause), 0, None));
}

#[test]
fn no_redirects_default_deny_blocks_follow_redirects_request() {
    // Closes ThreatModel T10. Omitting `no_redirects` selects the
    // strict / fail-safe interpretation: a rule must explicitly opt
    // into redirect-following trust to auto-allow such a request.
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.follow_redirects = true;
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    assert!(!matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn no_redirects_default_deny_admits_non_redirect_request() {
    // Default-deny only bites when the request itself is set to
    // follow redirects; unaffected requests still match.
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    assert!(matches_rule(
        &parsed,
        &http_rule("ok", allow_clause()),
        0,
        None
    ));
}

#[test]
fn no_redirects_explicit_true_blocks_follow_redirects_request() {
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.follow_redirects = true;
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    let mut clause = allow_clause();
    clause.no_redirects = Some(true);
    assert!(!matches_rule(&parsed, &http_rule("ok", clause), 0, None));
}

#[test]
fn no_redirects_false_admits_follow_redirects_request() {
    // Opt-in: rules that genuinely need redirect-following trust
    // signal it explicitly via `no_redirects: false`.
    let mut req = http(HttpMethod::Get, "https://example.test/v1/foo");
    req.follow_redirects = true;
    let parsed = parsed_with(vec![Effect::HttpRequest(req)]);
    let mut clause = allow_clause();
    clause.no_redirects = Some(false);
    assert!(matches_rule(
        &parsed,
        &http_rule("ok", clause.clone()),
        0,
        None
    ));

    // And still admits the non-redirect case.
    let parsed_non_redir = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    assert!(matches_rule(
        &parsed_non_redir,
        &http_rule("ok", clause),
        0,
        None
    ));
}

#[test]
fn command_filter_narrows_match() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let mut rule = http_rule("ok", allow_clause());
    rule.command = Some("curl".into());
    assert!(matches_rule(&parsed, &rule, 0, None));
    rule.command = Some("noop".into());
    assert!(!matches_rule(&parsed, &rule, 0, None));
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
        expires_at: None,
        sid: None,
    };
    assert!(!matches_rule(&parsed, &rule, 0, None));
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
        expires_at: None,
        sid: None,
    };
    assert!(!matches_rule(&parsed_one, &multi, 0, None));

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
    assert!(matches_rule(&parsed_both, &multi, 0, None));
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
        expires_at: None,
        sid: None,
    };
    assert!(!matches_rule(&parsed, &rule, 0, None));
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
    match decide(&parsed, &store, 0, None) {
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
    match decide(&parsed, &store, 0, None) {
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
    let d = decide(&parsed, &store, 0, None);
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
    let d = decide(&parsed, &store_no_project, 0, None);
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
    let d = decide(&parsed, &store_only_builtin, 0, None);
    assert!(
        matches!(&d, Decision::Allow { rule_id, scope: Scope::Builtin } if rule_id == "from-builtin"),
        "{d:?}",
    );
}

#[test]
fn expiry_boundary_now_equal_to_expires_at_fails() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let mut rule = http_rule("ok", allow_clause());
    rule.expires_at = Some(1_000);
    // Strictly before: matches.
    assert!(matches_rule(&parsed, &rule, 999, None));
    // `now == expires_at`: no longer matches — the rule's window is
    // `[created, expires_at)`, not inclusive of the boundary.
    assert!(!matches_rule(&parsed, &rule, 1_000, None));
    // Strictly after: still doesn't match.
    assert!(!matches_rule(&parsed, &rule, 1_001, None));
}

#[test]
fn sid_match_and_mismatch() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let mut rule = http_rule("ok", allow_clause());
    rule.sid = Some(42);
    assert!(matches_rule(&parsed, &rule, 0, Some(42)));
    assert!(!matches_rule(&parsed, &rule, 0, Some(43)));
    // A rule with `sid: Some(_)` never matches an unknown caller.
    assert!(!matches_rule(&parsed, &rule, 0, None));
}

#[test]
fn sid_unset_matches_regardless_of_caller_sid() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let rule = http_rule("ok", allow_clause());
    assert!(matches_rule(&parsed, &rule, 0, Some(42)));
    assert!(matches_rule(&parsed, &rule, 0, None));
}

#[test]
fn expired_session_rule_falls_through_to_prompt() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let mut expired = http_rule("session-rule", allow_clause());
    expired.expires_at = Some(100);
    expired.sid = Some(7);
    let store = AllowlistStore {
        denylist: vec![],
        session: vec![expired],
        project: vec![],
        user: vec![],
        builtin: vec![],
    };
    assert_eq!(decide(&parsed, &store, 200, Some(7)), Decision::Prompt);
}

#[test]
fn sid_mismatch_falls_through_to_prompt() {
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let mut session_rule = http_rule("session-rule", allow_clause());
    session_rule.sid = Some(7);
    let store = AllowlistStore {
        denylist: vec![],
        session: vec![session_rule],
        project: vec![],
        user: vec![],
        builtin: vec![],
    };
    assert_eq!(decide(&parsed, &store, 0, Some(99)), Decision::Prompt);
}

#[test]
fn denylist_wins_over_active_session_rule() {
    // Pins precedence: an active (non-expired, SID-matching) session
    // rule must not shadow a denylist hit.
    let parsed = parsed_with(vec![Effect::HttpRequest(http(
        HttpMethod::Get,
        "https://example.test/v1/foo",
    ))]);
    let mut session_rule = http_rule("session-allow", allow_clause());
    session_rule.sid = Some(7);
    let deny = http_rule("blocked", allow_clause());
    let store = AllowlistStore {
        denylist: vec![deny],
        session: vec![session_rule],
        project: vec![],
        user: vec![],
        builtin: vec![],
    };
    match decide(&parsed, &store, 0, Some(7)) {
        Decision::Deny { rule_id, scope } => {
            assert_eq!(rule_id, "blocked");
            assert_eq!(scope, Scope::Denylist);
        }
        other => panic!("expected deny, got {other:?}"),
    }
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
    assert_eq!(decide(&parsed, &store, 0, None), Decision::Prompt);
}
