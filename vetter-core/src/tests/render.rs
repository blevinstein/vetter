//! Tests for [`crate::render`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use crate::parsers::{
    Body, DisplayHints, Effect, Header, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
};
use url::Url;

fn render_to_string(p: &ParsedCommand) -> String {
    render_to_string_with(p, None)
}

fn render_to_string_with(p: &ParsedCommand, outcome: Option<&Decision>) -> String {
    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(p, outcome, &mut PlainWriter(&mut buf))
        .expect("render");
    String::from_utf8(buf).expect("utf-8")
}

fn pc_get_with_headers(headers: Vec<(&str, &str)>) -> ParsedCommand {
    ParsedCommand {
        command: "noop".into(),
        argv: vec!["noop".into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: Url::parse("https://example.test/").unwrap(),
            headers: headers
                .into_iter()
                .map(|(n, v)| Header {
                    name: n.into(),
                    value: v.into(),
                })
                .collect(),
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

#[test]
fn authorization_header_is_redacted_in_plain_output() {
    let p = pc_get_with_headers(vec![("Authorization", "Bearer abcdef1234567890wxyzf3a2")]);
    let out = render_to_string(&p);
    assert!(
        !out.contains("abcdef1234567890wxyzf3a2"),
        "raw token leaked: {out}"
    );
    assert!(out.contains("••••"), "missing redaction marker: {out}");
    assert!(out.contains("f3a2"), "missing last-4: {out}");
}

#[test]
fn cookie_xapikey_proxyauth_xtoken_are_all_redacted() {
    for h in [
        "Cookie",
        "X-Api-Key",
        "Proxy-Authorization",
        "X-Vault-Token",
    ] {
        let p = pc_get_with_headers(vec![(h, "supersecret-value-1234")]);
        let out = render_to_string(&p);
        assert!(
            !out.contains("supersecret-value-1234"),
            "header {h} not redacted: {out}"
        );
    }
}

#[test]
fn benign_headers_are_shown_in_full() {
    let p = pc_get_with_headers(vec![
        ("Content-Type", "application/json"),
        ("Accept", "*/*"),
        ("User-Agent", "curl/8.4.0"),
    ]);
    let out = render_to_string(&p);
    assert!(out.contains("application/json"));
    assert!(out.contains("*/*"));
    assert!(out.contains("curl/8.4.0"));
}

#[test]
fn no_rule_match_line_is_present() {
    let p = pc_get_with_headers(vec![]);
    let out = render_to_string(&p);
    assert!(out.contains("Match:"));
    assert!(out.contains("no rule"));
}

#[test]
fn long_url_is_not_truncated() {
    let long_path =
        "/repos/foo/bar/issues/12345?state=open&labels=needs-review&since=2026-01-01T00:00:00Z";
    let url = format!("https://api.github.com{long_path}");
    let p = ParsedCommand {
        command: "noop".into(),
        argv: vec![],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: Url::parse(&url).unwrap(),
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
    };
    let out = render_to_string(&p);
    assert!(out.contains(long_path), "path was truncated: {out}");
}

#[test]
fn ansi_writer_emits_escape_codes_for_styled_chunks() {
    let p = pc_get_with_headers(vec![("Content-Type", "application/json")]);
    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(&p, None, &mut AnsiWriter(&mut buf))
        .unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("\x1b["), "no ANSI escape in styled output");
}

#[test]
fn match_line_dispatches_on_decision() {
    let p = pc_get_with_headers(vec![]);

    let allow = Decision::Allow {
        rule_id: "github-readonly".into(),
        scope: Scope::Project,
    };
    let out = render_to_string_with(&p, Some(&allow));
    assert!(out.contains("matched rule github-readonly"), "{out}");
    assert!(out.contains("(project)"), "{out}");

    let deny = Decision::Deny {
        rule_id: "no-prod-writes".into(),
        scope: Scope::Denylist,
    };
    let out = render_to_string_with(&p, Some(&deny));
    assert!(out.contains("denylist no-prod-writes"), "{out}");
    assert!(out.contains("(denylist)"), "{out}");

    let prompt = Decision::Prompt;
    let out = render_to_string_with(&p, Some(&prompt));
    assert!(out.contains("no rule"), "{out}");
}
