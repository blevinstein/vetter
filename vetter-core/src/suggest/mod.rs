//! Generalisation engine for "Allowlist…" / "Trust host…" suggestions.
//!
//! Given a [`crate::ParsedCommand`] (typically one the human is being
//! asked to approve), the engine emits a small ordered list of
//! [`RuleSuggestion`]s and [`HostSuggestion`]s ranked from tightest
//! to loosest. Callers (the macOS popover picker, a future
//! `vet allow suggest` CLI) present them as radio choices; the user
//! picks one and the daemon persists it via
//! [`crate::matcher::loader::add_rule`] /
//! [`crate::known_hosts::add_host`].
//!
//! ## Tier table
//!
//! Allowlist (HTTP):
//!
//! | Tier         | Rule shape                                              |
//! |--------------|---------------------------------------------------------|
//! | `Exact`      | method + scheme + host + exact path                     |
//! | `PathGlob`   | method + scheme + host + path with last segment → `*`   |
//! | `MethodHost` | method + scheme + host + `/**`                          |
//!
//! `PathGlob` is omitted when the request path has zero or one
//! segments (`/` or `/foo`) since the path-glob would either equal
//! the exact path (no information added) or duplicate `MethodHost`.
//!
//! Known-hosts:
//!
//! | Tier        | Pattern shape       |
//! |-------------|---------------------|
//! | `Exact`     | `host`              |
//! | `Wildcard`  | `*.parent.tld`      |
//!
//! `Wildcard` is omitted when:
//! - the host has fewer than three labels (`apex.tld` → no parent
//!   to wildcard against without inverting the apex/wildcard rule
//!   in [`crate::matcher::glob::matches_host`]);
//! - the leftmost label is `www` (wildcarding `*.example.com`
//!   from `www.example.com` would be surprising — the user almost
//!   always means just this one host);
//! - the host parses as an IP literal (no DNS hierarchy to
//!   wildcard);
//! - the host contains an underscore or other non-DNS character
//!   (defensive — the parser may have allowed an IDN we don't
//!   want to wildcard).
//!
//! Loopback hosts are skipped entirely (already trusted; emitting
//! a known-host entry would be noise).

use serde::{Deserialize, Serialize};

use crate::known_hosts::{KnownHostEntry, KnownHostsStore};
use crate::matcher::rule::{HostPattern, HttpClause, Rule, RuleWhen, UrlClause};
use crate::matcher::url::normalise;
use crate::{Effect, HttpMethod, HttpRequest, ParsedCommand};

/// Which generalisation tier produced an allowlist suggestion.
/// Carried on the wire so the picker UI can render the tier label
/// without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionTier {
    Exact,
    PathGlob,
    MethodHost,
}

impl SuggestionTier {
    pub fn as_str(self) -> &'static str {
        match self {
            SuggestionTier::Exact => "exact",
            SuggestionTier::PathGlob => "path-glob",
            SuggestionTier::MethodHost => "method+host",
        }
    }
}

/// One ranked allowlist-rule candidate. `rule` is fully formed and
/// can be passed straight to [`crate::matcher::loader::add_rule`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleSuggestion {
    pub tier: SuggestionTier,
    /// Short human-readable label, e.g. `"GET https://api.github.com/repos/foo/bar"`.
    /// The picker shows this above the YAML preview.
    pub label: String,
    pub rule: Rule,
}

/// Tier for known-host suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostTier {
    Exact,
    Wildcard,
}

impl HostTier {
    pub fn as_str(self) -> &'static str {
        match self {
            HostTier::Exact => "exact",
            HostTier::Wildcard => "wildcard",
        }
    }
}

/// One ranked known-host candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSuggestion {
    pub tier: HostTier,
    /// Short human-readable label, e.g. `"api.github.com"` or
    /// `"*.github.com"`.
    pub label: String,
    pub entry: KnownHostEntry,
}

/// Build allowlist-rule suggestions for `parsed`.
///
/// Picks the **first** [`Effect::HttpRequest`] in `parsed.effects`
/// (matches what the popover URL row already shows) and emits
/// 1–3 tiers ordered tightest → loosest. Returns an empty `Vec`
/// when there is no HTTP effect.
///
/// Rule ids are derived deterministically via
/// [`crate::matcher::derive_auto_id`] so re-clicking the same tier
/// produces the same id (and the duplicate-id check in the loader
/// short-circuits a second click instead of accumulating
/// near-identical rules).
pub fn allowlist_suggestions(parsed: &ParsedCommand) -> Vec<RuleSuggestion> {
    let req = match parsed.effects.iter().find_map(|e| match e {
        Effect::HttpRequest(req) => Some(req),
        _ => None,
    }) {
        Some(req) => req,
        None => return Vec::new(),
    };

    let normalised = normalise(&req.url);
    let scheme = normalised.scheme().to_string();
    let host = match normalised.host_str() {
        Some(h) => h.to_string(),
        None => return Vec::new(),
    };
    let path = normalised.path().to_string();
    let method = req.method.clone();

    let mut out = Vec::with_capacity(3);

    // Tier 1: exact path.
    out.push(make_rule_suggestion(
        SuggestionTier::Exact,
        &method,
        &scheme,
        &host,
        Some(path.clone()),
    ));

    // Tier 2: path-glob with last segment replaced by `*`.
    // Skip the trivial cases: no segments (root) or a single segment
    // (the glob would be `/*`, which `MethodHost` already covers
    // more explicitly).
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let non_empty_segments: Vec<&str> =
        segments.iter().copied().filter(|s| !s.is_empty()).collect();
    if non_empty_segments.len() >= 2 {
        let mut glob_segments: Vec<&str> = non_empty_segments.clone();
        let last = glob_segments.len() - 1;
        glob_segments[last] = "*";
        let glob_path = format!("/{}", glob_segments.join("/"));
        // Only emit when the glob differs from the exact path; if
        // the last segment was already `*` (won't happen with a real
        // request URL) the suggestion would be redundant.
        if glob_path != path {
            out.push(make_rule_suggestion(
                SuggestionTier::PathGlob,
                &method,
                &scheme,
                &host,
                Some(glob_path),
            ));
        }
    }

    // Tier 3: method + host only (`path: /**`).
    out.push(make_rule_suggestion(
        SuggestionTier::MethodHost,
        &method,
        &scheme,
        &host,
        Some("/**".to_string()),
    ));

    out
}

/// Build known-host suggestions for `parsed`.
///
/// Returns an empty `Vec` when the host is loopback or already
/// matches an entry in `store` (any layer). See module docs for
/// the per-tier emission rules.
pub fn host_suggestions(parsed: &ParsedCommand, store: &KnownHostsStore) -> Vec<HostSuggestion> {
    let req = match parsed.effects.iter().find_map(|e| match e {
        Effect::HttpRequest(req) => Some(req),
        _ => None,
    }) {
        Some(req) => req,
        None => return Vec::new(),
    };
    let host = match req.url.host_str() {
        Some(h) => h.to_lowercase(),
        None => return Vec::new(),
    };

    if is_loopback_host(&host) {
        return Vec::new();
    }
    if store.contains(&host) {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(2);
    out.push(HostSuggestion {
        tier: HostTier::Exact,
        label: host.clone(),
        entry: KnownHostEntry {
            pattern: host.clone(),
            note: None,
        },
    });

    if let Some(wildcard) = wildcard_pattern(&host) {
        // Avoid duplicate when the store already covers the wildcard
        // form via a leading `*.` entry (rare — `contains` uses the
        // request host, not the wildcard pattern, so we re-check
        // explicitly here).
        if !store.contains(&strip_wildcard_prefix(&wildcard)) {
            out.push(HostSuggestion {
                tier: HostTier::Wildcard,
                label: wildcard.clone(),
                entry: KnownHostEntry {
                    pattern: wildcard,
                    note: None,
                },
            });
        }
    }

    out
}

fn strip_wildcard_prefix(pattern: &str) -> String {
    pattern
        .strip_prefix("*.")
        .map(|s| s.to_string())
        .unwrap_or_else(|| pattern.to_string())
}

/// Build the `*.parent.tld` form for `host`. Returns `None` when no
/// suitable parent exists (see module docs for the per-condition
/// rationale).
fn wildcard_pattern(host: &str) -> Option<String> {
    if is_ip_literal(host) {
        return None;
    }
    if !is_safe_dns_name(host) {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 3 {
        return None;
    }
    if labels[0].eq_ignore_ascii_case("www") {
        return None;
    }
    let parent = labels[1..].join(".");
    Some(format!("*.{parent}"))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1") || host.starts_with("127.") || host == "[::1]"
}

fn is_ip_literal(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    // Bracketed v6 form, as `Url::host_str` may return either.
    if let Some(stripped) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        if stripped.parse::<std::net::Ipv6Addr>().is_ok() {
            return true;
        }
    }
    false
}

/// Defensive filter that rejects hosts containing characters we
/// don't want to wildcard-match. DNS labels are
/// `[A-Za-z0-9-]` plus dots between them. We additionally accept
/// IDN-style hosts (`xn--…`) which fall under the alphanumeric
/// rule, but reject anything with `_`, `:` (port leakage), `/`
/// (path leakage), etc. The exact-tier suggestion is unaffected;
/// only the wildcard tier is gated.
fn is_safe_dns_name(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

fn make_rule_suggestion(
    tier: SuggestionTier,
    method: &HttpMethod,
    scheme: &str,
    host: &str,
    path: Option<String>,
) -> RuleSuggestion {
    let url_clause = UrlClause {
        scheme: Some(scheme.to_string()),
        host: Some(HostPattern::One(host.to_string())),
        port: None,
        path,
    };
    let http = HttpClause {
        method: Some(vec![method.clone()]),
        url: Some(url_clause.clone()),
        headers_allow: None,
        no_body: None,
        query: None,
    };
    let rule_when = RuleWhen {
        http: Some(http),
        file_write: None,
        file_read: None,
    };
    let mut rule = Rule {
        id: String::new(),
        command: None,
        when: rule_when,
        note: None,
        created_by: None,
        created_at: None,
    };
    rule.id = crate::matcher::derive_auto_id(&rule);

    let label = format!(
        "{} {}://{}{}",
        method.as_str(),
        scheme,
        host,
        url_clause.path.as_deref().unwrap_or("/")
    );

    RuleSuggestion { tier, label, rule }
}

/// Marker trait so this module can extract a request without
/// duplicating the "first HTTP effect" lookup elsewhere. Currently
/// only used internally.
#[allow(dead_code)]
fn first_http(parsed: &ParsedCommand) -> Option<&HttpRequest> {
    parsed.effects.iter().find_map(|e| match e {
        Effect::HttpRequest(req) => Some(req),
        _ => None,
    })
}

#[cfg(test)]
#[path = "../tests/suggest.rs"]
mod tests;
