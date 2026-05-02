//! Layered decision algorithm: denylist > session > project > user > built-in.
//!
//! [`decide`] is the only function callers need. It walks the
//! [`AllowlistStore`] in scope-precedence order, returning the first
//! matching rule. With no match anywhere it returns
//! [`Decision::Prompt`] — the default-deny outcome that the daemon
//! later turns into an interactive approval flow.
//!
//! [`matches_rule`] is the per-rule predicate, also exposed because the
//! property-test suite drives it directly.

use serde::{Deserialize, Serialize};

use crate::matcher::glob::{matches_host, matches_path};
use crate::matcher::loader::AllowlistStore;
use crate::matcher::rule::{
    FileReadClause, FileWriteClause, HostPattern, HttpClause, Rule, UrlClause,
};
use crate::matcher::url::normalise;
use crate::{Body, Effect, FileRead, FileWrite, Header, HttpRequest, ParsedCommand};

/// The outcome of evaluating a [`ParsedCommand`] against an
/// [`AllowlistStore`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    Allow {
        rule_id: String,
        scope: Scope,
    },
    Deny {
        rule_id: String,
        scope: Scope,
    },
    /// No rule matched; the daemon will eventually escalate to a user
    /// prompt. Until the daemon ships this is the explicit "no rule"
    /// outcome the renderer prints in yellow.
    Prompt,
}

/// Which allowlist layer produced the matching rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Builtin,
    User,
    Project,
    Session,
    Denylist,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Builtin => "built-in",
            Scope::User => "user",
            Scope::Project => "project",
            Scope::Session => "session",
            Scope::Denylist => "denylist",
        }
    }
}

/// Walk the store and return the first rule that fires under the
/// layered precedence rules from `plans/Overview.md` §2.
pub fn decide(parsed: &ParsedCommand, store: &AllowlistStore) -> Decision {
    for r in &store.denylist {
        if matches_rule(parsed, r) {
            return Decision::Deny {
                rule_id: r.id.clone(),
                scope: Scope::Denylist,
            };
        }
    }
    for (scope, rules) in [
        (Scope::Session, &store.session),
        (Scope::Project, &store.project),
        (Scope::User, &store.user),
        (Scope::Builtin, &store.builtin),
    ] {
        if let Some(r) = rules.iter().find(|r| matches_rule(parsed, r)) {
            return Decision::Allow {
                rule_id: r.id.clone(),
                scope,
            };
        }
    }
    Decision::Prompt
}

/// True when `rule` fires for `parsed`.
///
/// A populated effect-clause must find at least **one** matching effect
/// in `parsed.effects`. Multiple populated clauses must all be satisfied
/// (each by some effect — not necessarily the same one). Empty
/// [`crate::matcher::RuleWhen`] never matches; the loader rejects such
/// rules at load time so this is purely defensive.
pub fn matches_rule(parsed: &ParsedCommand, rule: &Rule) -> bool {
    if let Some(cmd) = &rule.command {
        if cmd != &parsed.command {
            return false;
        }
    }
    if rule.when.is_empty() {
        return false;
    }
    if let Some(http) = &rule.when.http {
        if !parsed
            .effects
            .iter()
            .any(|e| matches!(e, Effect::HttpRequest(req) if matches_http(req, http)))
        {
            return false;
        }
    }
    if let Some(fw) = &rule.when.file_write {
        if !parsed
            .effects
            .iter()
            .any(|e| matches!(e, Effect::FileWrite(w) if matches_file_write(w, fw)))
        {
            return false;
        }
    }
    if let Some(fr) = &rule.when.file_read {
        if !parsed
            .effects
            .iter()
            .any(|e| matches!(e, Effect::FileRead(r) if matches_file_read(r, fr)))
        {
            return false;
        }
    }
    true
}

fn matches_http(req: &HttpRequest, clause: &HttpClause) -> bool {
    if let Some(methods) = &clause.method {
        if !methods.iter().any(|m| m == &req.method) {
            return false;
        }
    }
    if let Some(url_clause) = &clause.url {
        if !matches_url(req, url_clause) {
            return false;
        }
    }
    if !matches_headers(&req.headers, clause.headers_allow.as_deref()) {
        return false;
    }
    if let Some(no_body) = clause.no_body {
        if no_body && !matches!(req.body, Body::None) {
            return false;
        }
    }
    if clause.query.unwrap_or(false) {
        let normalised = normalise(&req.url);
        if normalised.query().unwrap_or("").is_empty() {
            return false;
        }
    }
    true
}

fn matches_url(req: &HttpRequest, clause: &UrlClause) -> bool {
    let normalised = normalise(&req.url);
    if let Some(scheme) = &clause.scheme {
        if !scheme.eq_ignore_ascii_case(normalised.scheme()) {
            return false;
        }
    }
    if let Some(host_pat) = &clause.host {
        let host = normalised.host_str().unwrap_or("");
        if !any_host_matches(host_pat, host) {
            return false;
        }
    }
    if let Some(ports) = &clause.port {
        let port = normalised
            .port_or_known_default()
            .or_else(|| normalised.port());
        if !port.map(|p| ports.contains(&p)).unwrap_or(false) {
            return false;
        }
    }
    if let Some(path_pat) = &clause.path {
        if !matches_path(path_pat, normalised.path()) {
            return false;
        }
    }
    true
}

fn any_host_matches(pat: &HostPattern, host: &str) -> bool {
    pat.patterns().iter().any(|p| matches_host(p, host))
}

/// Default-deny header semantics. `None` (clause omitted) or `Some([])`
/// → reject any request that has any headers. `Some(["*"])` short-
/// circuits to allow any header. Otherwise every header on the request
/// must appear (case-insensitive) in the whitelist.
fn matches_headers(headers: &[Header], allow: Option<&[String]>) -> bool {
    let Some(allow) = allow else {
        return headers.is_empty();
    };
    if allow.iter().any(|h| h == "*") {
        return true;
    }
    if allow.is_empty() {
        return headers.is_empty();
    }
    headers
        .iter()
        .all(|h| allow.iter().any(|a| a.eq_ignore_ascii_case(&h.name)))
}

fn matches_file_write(w: &FileWrite, clause: &FileWriteClause) -> bool {
    matches_optional_path_glob(&w.path.to_string_lossy(), clause.path.as_deref())
}

fn matches_file_read(r: &FileRead, clause: &FileReadClause) -> bool {
    matches_optional_path_glob(&r.path.to_string_lossy(), clause.path.as_deref())
}

fn matches_optional_path_glob(path: &str, pattern: Option<&str>) -> bool {
    match pattern {
        Some(pat) => {
            let normalised = crate::matcher::url::normalise_path(path);
            matches_path(pat, &normalised)
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
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
}
