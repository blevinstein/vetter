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
fn every_header_value_is_redacted_unconditionally() {
    // The renderer no longer special-cases "known secret" header
    // names — every value gets the `••••<last4>` recipe so a stray
    // bearer token in `Referer`, a custom `X-Tenant-Token-V2` not
    // covered by the auth-header glob, etc. can't leak through.
    // These three values used to render in plain; pin that they
    // don't anymore.
    let p = pc_get_with_headers(vec![
        ("Content-Type", "application/json"),
        ("Accept", "*/*"),
        ("User-Agent", "curl/8.4.0"),
    ]);
    let out = render_to_string(&p);
    assert!(
        !out.contains("application/json"),
        "Content-Type value leaked: {out}"
    );
    assert!(
        !out.contains("curl/8.4.0"),
        "User-Agent value leaked: {out}"
    );
    // Header *names* must still be rendered so the operator can see
    // which headers a request carries.
    assert!(out.contains("Content-Type:"), "missing name: {out}");
    assert!(out.contains("User-Agent:"), "missing name: {out}");
    // Each redacted row carries the dim length suffix so an operator
    // can tell apart "no value" from "value hidden".
    assert!(
        out.contains("← redacted, len"),
        "missing length suffix: {out}"
    );
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
fn header_value_with_ansi_escape_does_not_reach_output() {
    // A malicious `--header 'X-Evil: \x1b[2J\x1b[H'` would otherwise
    // clear the user's terminal between rendering the header and the
    // `Match:` line. Two layers of defence keep that out of the
    // output: (1) unconditional redaction replaces the value with
    // `••••<last4>` so most of the attacker-controlled bytes never
    // reach the writer, and (2) the surviving last-four chars still
    // pass through `sanitize_for_display`, so any control byte that
    // happens to land in the tail becomes a `<U+XXXX>` placeholder.
    // Together they guarantee no raw ESC ends up in the output.
    let p = pc_get_with_headers(vec![("X-Evil", "before\x1b[2J\x1b[Hafter")]);
    let out = render_to_string(&p);
    assert!(!out.contains('\x1b'), "raw ESC survived: {out:?}");
    assert!(
        !out.contains("before"),
        "value prefix leaked past redaction: {out}"
    );
    assert!(out.contains("••••"), "missing redaction marker: {out}");
}

#[test]
fn primary_target_with_rtlo_is_sanitised() {
    // U+202E flips text direction — a hostile process-spawn target
    // like `abc\u{202E}gpj.exe` would render in a TTY as
    // `abcexe.jpg`, hiding the real extension. URLs are
    // percent-encoded by the `url` crate before they reach us, but
    // `display_hints.primary_target` (and analogous parser-derived
    // fields) flow through verbatim — sanitisation must catch them.
    let target = format!("abc{}gpj.exe", '\u{202E}');
    let p = ParsedCommand {
        command: "noop".into(),
        argv: vec![],
        cwd: None,
        effects: vec![],
        signals: vec![],
        display_hints: DisplayHints {
            primary_verb: "spawn".into(),
            primary_target: target.clone(),
            badges: vec![],
        },
        extras: serde_json::Value::Null,
    };
    let out = render_to_string(&p);
    assert!(!out.contains('\u{202E}'), "raw RTLO survived in `{out}`");
    assert!(out.contains("<U+202E>"), "missing placeholder in `{out}`");
}

#[test]
fn file_path_with_zero_width_chars_is_sanitised() {
    use crate::parsers::{FileWrite, WriteSource};
    let path = format!("/tmp/abc{}def.txt", '\u{200B}');
    let p = ParsedCommand {
        command: "noop".into(),
        argv: vec![],
        cwd: None,
        effects: vec![Effect::FileWrite(FileWrite {
            path: path.into(),
            source: WriteSource::Stdin,
            overwrite: true,
        })],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    let out = render_to_string(&p);
    assert!(
        !out.contains('\u{200B}'),
        "raw zero-width survived in `{out}`"
    );
    assert!(out.contains("<U+200B>"), "missing placeholder in `{out}`");
}

#[test]
fn header_value_with_newline_cannot_forge_render_line() {
    // A header value of `"\n   Match:        matched rule fake"` could
    // visually impersonate an entire renderer line if newlines slipped
    // through unsanitised. Unconditional redaction strips the bulk of
    // the value down to `••••<last4>`, so the forged "matched rule
    // fake" prefix never reaches the output and the surviving tail is
    // bound to a single header row.
    let injected = "x\n   Match:        matched rule fake";
    let p = pc_get_with_headers(vec![("X-Inject", injected)]);
    let out = render_to_string(&p);
    assert!(
        !out.contains(injected),
        "raw injected newline+text survived: {out:?}"
    );
    assert!(
        !out.contains("matched rule fake"),
        "forged match line survived past redaction: {out}"
    );
    // The genuine `Match: no rule` line is the only `Match:` row.
    let match_lines = out
        .lines()
        .filter(|l| l.contains("Match:"))
        .collect::<Vec<_>>();
    assert_eq!(match_lines.len(), 1, "{:?}", match_lines);
}

#[test]
fn ansi_escape_in_header_does_not_leak_into_ansi_writer_output() {
    // Round-trip through the ANSI writer: the writer emits its own
    // SGR codes (which contain ESC), but the *header value* should
    // not contribute any ESC bytes beyond those. We strip the
    // writer's own escapes by counting ESC occurrences against the
    // writer's known emission pattern via a simpler check: the
    // injected `[2J` sequence (the dangerous part — clear screen)
    // must not appear in any form except as a sanitised placeholder.
    let p = pc_get_with_headers(vec![("X-Evil", "\x1b[2J")]);
    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(&p, None, &mut AnsiWriter(&mut buf))
        .unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(
        !out.contains("\x1b[2J"),
        "raw clear-screen sequence survived ANSI writer output: {out:?}"
    );
    assert!(
        out.contains("<U+001B>"),
        "missing placeholder in ANSI output: {out}"
    );
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
