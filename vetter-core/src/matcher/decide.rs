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
///
/// `now` is Unix-epoch seconds, checked against
/// [`crate::matcher::rule::Rule::expires_at`]. `caller_sid` is the
/// requesting connection's stable POSIX session id (see
/// [`crate::peer_cred::stable_session_for`]), checked against
/// [`crate::matcher::rule::Rule::sid`]; pass `None` when the caller
/// is unknown (e.g. `vet --explain`, which never sees the daemon's
/// live session rules).
pub fn decide(
    parsed: &ParsedCommand,
    store: &AllowlistStore,
    now: u64,
    caller_sid: Option<i32>,
) -> Decision {
    for r in &store.denylist {
        if matches_rule(parsed, r, now, caller_sid) {
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
        if let Some(r) = rules
            .iter()
            .find(|r| matches_rule(parsed, r, now, caller_sid))
        {
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
///
/// `now` / `caller_sid` gate [`Rule::expires_at`] / [`Rule::sid`]
/// respectively — see [`decide`]'s doc comment for their meaning.
pub fn matches_rule(
    parsed: &ParsedCommand,
    rule: &Rule,
    now: u64,
    caller_sid: Option<i32>,
) -> bool {
    if let Some(exp) = rule.expires_at {
        if now >= exp {
            return false;
        }
    }
    if let Some(sid) = rule.sid {
        if caller_sid != Some(sid) {
            return false;
        }
    }
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
    if clause.no_redirects.unwrap_or(true) && req.follow_redirects {
        return false;
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
#[path = "../tests/matcher_decide.rs"]
mod tests;
