//! Rule schema — see `plans/Overview.md` §5 and `plans/phase2*.plan.md`.
//!
//! Rules are deserialised from YAML files (see [`crate::matcher::loader`])
//! and queried by [`crate::matcher::decide`]. Every clause is optional;
//! omitted clauses are not evaluated. A rule with **no** clauses set on
//! `when` matches every command — the loader rejects such a rule.

use serde::{Deserialize, Serialize};

use crate::HttpMethod;

/// One named allow-or-deny entry. The discrimination between allow and
/// deny is positional in the YAML (`rules:` vs `deny:`) — a rule itself
/// carries no allow/deny flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    /// Restrict the rule to a single command parser (e.g. `"curl"`).
    /// `None` means it applies to any parser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub when: RuleWhen,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Unix epoch seconds. Once `now >= expires_at`, this rule stops
    /// matching anywhere it's evaluated. Written by the popover's
    /// duration picker for anything shorter than "Forever"; nothing
    /// stops a human from hand-authoring one too. `#[serde(default)]`
    /// keeps every existing rule (and every existing `Rule { .. }`
    /// literal) parsing/compiling unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// POSIX session id ([`crate::peer_cred::stable_session_for`]).
    /// Once set, this rule only matches requests whose caller shares
    /// this session — see [`crate::matcher::decide::matches_rule`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<i32>,
}

/// Effect-keyed clause set. Every populated clause must find at least
/// one matching effect on the parsed command for the rule to fire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleWhen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpClause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_write: Option<FileWriteClause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_read: Option<FileReadClause>,
}

impl RuleWhen {
    /// True if no clause is set at all.
    pub fn is_empty(&self) -> bool {
        self.http.is_none() && self.file_write.is_none() && self.file_read.is_none()
    }
}

/// Predicate against [`crate::Effect::HttpRequest`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpClause {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Vec<HttpMethod>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<UrlClause>,
    /// Whitelist of header names allowed on the request. **Default-deny**:
    /// `None` or `Some(empty)` → reject any request that carries any
    /// header. The single-element `["*"]` is the only wildcard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers_allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_body: Option<bool>,
    /// Cross-cutting open question (Overview): query strings are off by
    /// default. `None` or `Some(false)` → query is ignored. `Some(true)`
    /// → request must have a non-empty query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<bool>,
    /// Redirect-following predicate. **Default-deny**: `None` or
    /// `Some(true)` → the rule does NOT match a request whose
    /// [`crate::HttpRequest::follow_redirects`] is `true` (e.g. curl
    /// `-L` / `--location`). `Some(false)` → the rule matches
    /// regardless of `follow_redirects`. Mirrors the `headers_allow`
    /// pattern: omitting the predicate selects the strict / fail-safe
    /// interpretation, so existing rules don't silently auto-allow a
    /// host that open-redirects (or is compromised) to an arbitrary
    /// origin. See ThreatModel T10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_redirects: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlClause {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostPattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<Vec<u16>>,
    /// Glob: `*` matches one path segment, `**` matches any number of
    /// segments (including zero). Always matched against the
    /// **normalised** request path (`/admin/../secret` → `/secret`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// One-or-many host pattern. Each entry can be exact or a leading-`*.`
/// glob (e.g. `"*.github.com"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HostPattern {
    One(String),
    Many(Vec<String>),
}

impl HostPattern {
    /// Iterate the patterns regardless of which serde shape was used.
    pub fn patterns(&self) -> &[String] {
        match self {
            HostPattern::One(s) => std::slice::from_ref(s),
            HostPattern::Many(v) => v.as_slice(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteClause {
    /// Glob against the normalised target path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadClause {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[cfg(test)]
#[path = "../tests/matcher_rule.rs"]
mod tests;
