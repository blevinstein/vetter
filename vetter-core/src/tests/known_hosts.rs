//! Tests for [`crate::known_hosts`] and the associated
//! [`crate::signals::check_known_hosts`] signal emitter.
//! Layout convention described in `AGENTS.md`.

// `super::*` exposes everything from `crate::known_hosts` (KnownHostEntry,
// KnownHostsStore, load_file, …) since this file is `mod tests` inside
// `known_hosts.rs`.
use super::*;
use crate::parsers::{
    Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
};
use crate::signals::{check_known_hosts, SignalKind};
use url::Url;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn store_with(patterns: &[&str]) -> KnownHostsStore {
    KnownHostsStore {
        builtin: vec![],
        user: patterns
            .iter()
            .map(|p| KnownHostEntry {
                pattern: p.to_string(),
                note: None,
            })
            .collect(),
        project: vec![],
    }
}

fn pc_with_url(url: &str) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into(), url.into()],
        cwd: Some("/work".into()),
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: Url::parse(url).unwrap(),
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

// ---------------------------------------------------------------------------
// KnownHostsStore::contains
// ---------------------------------------------------------------------------

#[test]
fn contains_exact_match_case_insensitive() {
    let store = store_with(&["api.github.com"]);
    assert!(store.contains("api.github.com"));
    assert!(store.contains("API.GITHUB.COM"));
    assert!(!store.contains("api.gitlab.com"));
}

#[test]
fn contains_wildcard_matches_subdomain_not_apex() {
    let store = store_with(&["*.github.com"]);
    assert!(store.contains("api.github.com"));
    assert!(!store.contains("raw.githubusercontent.com"));
    // apex itself must NOT match the `*.` pattern
    assert!(!store.contains("github.com"));
    // deep subdomain should match
    assert!(store.contains("deeply.nested.github.com"));
}

#[test]
fn contains_checks_all_layers() {
    let store = KnownHostsStore {
        builtin: vec![KnownHostEntry {
            pattern: "builtin.example.com".into(),
            note: None,
        }],
        user: vec![KnownHostEntry {
            pattern: "user.example.com".into(),
            note: None,
        }],
        project: vec![KnownHostEntry {
            pattern: "project.example.com".into(),
            note: None,
        }],
    };
    assert!(store.contains("builtin.example.com"));
    assert!(store.contains("user.example.com"));
    assert!(store.contains("project.example.com"));
    assert!(!store.contains("other.example.com"));
}

#[test]
fn builtin_only_has_known_entries() {
    let store = KnownHostsStore::builtin_only();
    // A selection from the static BUILTIN_HOSTS list
    assert!(store.contains("api.github.com"));
    assert!(store.contains("registry.npmjs.org"));
    assert!(store.contains("pypi.org"));
    assert!(store.contains("crates.io"));
    assert!(store.contains("api.openai.com"));
    // Verify wildcard builtins work too
    assert!(store.contains("myproject.googleapis.com"));
    assert!(store.contains("mybucket.s3.amazonaws.com"));
    // Unknown host not in the builtin list
    assert!(!store.contains("evil.example.com"));
}

// ---------------------------------------------------------------------------
// YAML loading
// ---------------------------------------------------------------------------

#[test]
fn load_file_parses_valid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known-hosts.yaml");
    std::fs::write(
        &path,
        r#"
hosts:
  - pattern: "api.example.com"
    note: "Example API"
  - pattern: "*.example.org"
"#,
    )
    .unwrap();
    let file = load_file(&path).unwrap();
    assert_eq!(file.hosts.len(), 2);
    assert_eq!(file.hosts[0].pattern, "api.example.com");
    assert_eq!(file.hosts[0].note.as_deref(), Some("Example API"));
    assert_eq!(file.hosts[1].pattern, "*.example.org");
    assert!(file.hosts[1].note.is_none());
}

#[test]
fn load_file_empty_hosts_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known-hosts.yaml");
    std::fs::write(&path, "hosts: []\n").unwrap();
    let file = load_file(&path).unwrap();
    assert!(file.hosts.is_empty());
}

#[test]
fn load_file_missing_hosts_key_defaults_to_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known-hosts.yaml");
    std::fs::write(&path, "{}\n").unwrap();
    let file = load_file(&path).unwrap();
    assert!(file.hosts.is_empty());
}

// ---------------------------------------------------------------------------
// check_known_hosts signal emission
// ---------------------------------------------------------------------------

#[test]
fn unknown_host_signal_emitted_for_unlisted_host() {
    let store = store_with(&["api.example.com"]);
    let p = pc_with_url("https://other.example.com/path");
    let sigs = check_known_hosts(&p, &store);
    assert_eq!(sigs.len(), 1);
    assert_eq!(sigs[0].kind, SignalKind::UnknownHost);
    assert!(sigs[0].detail.contains("other.example.com"));
    assert_eq!(sigs[0].effect_idx, Some(0));
}

#[test]
fn no_signal_for_known_host() {
    let store = store_with(&["api.example.com"]);
    let p = pc_with_url("https://api.example.com/v1");
    let sigs = check_known_hosts(&p, &store);
    assert!(sigs.is_empty());
}

#[test]
fn no_signal_for_wildcard_match() {
    let store = store_with(&["*.example.com"]);
    let p = pc_with_url("https://api.example.com/v1");
    let sigs = check_known_hosts(&p, &store);
    assert!(sigs.is_empty());
}

#[test]
fn loopback_localhost_never_emits_signal() {
    let store = KnownHostsStore::default(); // empty — no known hosts at all
    for url in &[
        "http://localhost:3000/",
        "http://127.0.0.1:8080/",
        "http://[::1]/",
    ] {
        let p = pc_with_url(url);
        let sigs = check_known_hosts(&p, &store);
        assert!(
            sigs.is_empty(),
            "expected no UnknownHost signal for loopback {url}, got {sigs:?}"
        );
    }
}

#[test]
fn builtin_hosts_dont_emit_signal() {
    let store = KnownHostsStore::builtin_only();
    for url in &[
        "https://api.github.com/repos/foo/bar",
        "https://registry.npmjs.org/express",
        "https://pypi.org/simple/requests/",
        "https://crates.io/api/v1/crates/serde",
        "https://api.openai.com/v1/chat/completions",
    ] {
        let p = pc_with_url(url);
        let sigs = check_known_hosts(&p, &store);
        assert!(
            sigs.is_empty(),
            "expected no UnknownHost for builtin host in {url}, got {sigs:?}"
        );
    }
}

#[test]
fn empty_effects_produces_no_signals() {
    let store = KnownHostsStore::default();
    let p = ParsedCommand {
        command: "curl".into(),
        argv: vec![],
        cwd: None,
        stdin_digest: None,
        effects: vec![],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    assert!(check_known_hosts(&p, &store).is_empty());
}
