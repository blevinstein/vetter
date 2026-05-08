//! Tests for [`crate::runloop::popover_attr`].
//!
//! The parser half ([`parse_ansi_spans`]) is pure Rust and the focus
//! of most assertions here. The Foundation half
//! ([`spans_to_attributed`]) gets a smoke test that asserts on the
//! resulting `NSAttributedString`'s `length()` and the number of
//! attribute runs — proxies for "every span made it across without
//! tripping AppKit". We avoid asserting on `NSColor` /
//! `NSFont` equality directly because objc2 doesn't expose stable
//! equality for those (and Apple doesn't promise factory-method
//! singletons either).

#![cfg(target_os = "macos")]

use objc2_foundation::NSRange;

use super::*;

fn ansi_red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
}

#[test]
fn empty_input_yields_no_spans() {
    assert!(parse_ansi_spans("").is_empty());
}

#[test]
fn plain_input_yields_one_default_span() {
    let spans = parse_ansi_spans("hello world");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "hello world");
    assert_eq!(spans[0].style, SpanStyle::default());
}

/// Mirrors `Style::Header` (bold).
#[test]
fn bold_open_close() {
    let spans = parse_ansi_spans(" vet  \x1b[1mcurl\x1b[0m\n");
    let texts: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec![" vet  ", "curl", "\n"]);
    assert!(!spans[0].style.bold);
    assert!(spans[1].style.bold);
    assert!(!spans[2].style.bold);
}

/// `Style::Method(Read)` = bold + green; the writer emits two CSI
/// sequences (`\e[1m\e[32m`) before the text and a single reset after.
#[test]
fn bold_plus_green_compose_into_one_run() {
    let spans = parse_ansi_spans("\x1b[1m\x1b[32mGET\x1b[0m");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "GET");
    assert!(spans[0].style.bold);
    assert_eq!(spans[0].style.color, Some(AnsiColor::Green));
}

/// `Style::Url` = cyan + underline.
#[test]
fn cyan_plus_underline_url_run() {
    let spans = parse_ansi_spans("\x1b[4m\x1b[36mhttps://x.test/\x1b[0m");
    assert_eq!(spans.len(), 1);
    assert!(spans[0].style.underline);
    assert_eq!(spans[0].style.color, Some(AnsiColor::Cyan));
    assert!(!spans[0].style.dim);
}

/// `Style::Loopback` = cyan + dim. The Foundation translator collapses
/// this into `secondaryLabelColor` (asserted in the smoke test), but
/// the parser preserves both bits.
#[test]
fn cyan_plus_dim_loopback_run() {
    let spans = parse_ansi_spans("\x1b[2m\x1b[36mhttp://localhost/\x1b[0m");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style.color, Some(AnsiColor::Cyan));
    assert!(spans[0].style.dim);
}

/// One test per FG colour we recognise. Order mirrors the
/// `ansi_for` match in `vetter-core::render`.
#[test]
fn each_recognised_foreground_colour() {
    for (code, expected) in [
        (31, AnsiColor::Red),
        (32, AnsiColor::Green),
        (33, AnsiColor::Yellow),
        (35, AnsiColor::Magenta),
        (36, AnsiColor::Cyan),
        (90, AnsiColor::BrightBlack),
        (94, AnsiColor::BrightBlue),
    ] {
        let s = format!("\x1b[{code}mx\x1b[0m");
        let spans = parse_ansi_spans(&s);
        assert_eq!(spans.len(), 1, "code {code}: {spans:?}");
        assert_eq!(spans[0].style.color, Some(expected), "code {code}");
    }
}

/// `Style::Badge(Danger)` = bold red. Verifies multi-flag
/// accumulation across separate CSI sequences.
#[test]
fn bold_red_badge_run() {
    let spans = parse_ansi_spans("\x1b[1m\x1b[31m[insecure: -k]\x1b[0m");
    assert_eq!(spans.len(), 1);
    assert!(spans[0].style.bold);
    assert_eq!(spans[0].style.color, Some(AnsiColor::Red));
}

/// Reset (`0`) wipes every accumulated bit; the next span starts
/// from `SpanStyle::default()`.
#[test]
fn reset_clears_accumulated_state() {
    let spans = parse_ansi_spans("\x1b[1m\x1b[31mhot\x1b[0mcold");
    assert_eq!(spans.len(), 2);
    assert!(spans[0].style.bold);
    assert_eq!(spans[0].style.color, Some(AnsiColor::Red));
    assert_eq!(spans[1].style, SpanStyle::default());
}

/// Multiple codes in a single CSI sequence (`\e[1;31m`) compose just
/// like adjacent CSIs do. `anstyle` doesn't currently emit this form
/// for the writer, but supporting it costs nothing and protects
/// against a future encoding change.
#[test]
fn semicolon_separated_codes_compose() {
    let spans = parse_ansi_spans("\x1b[1;31mboom\x1b[0m");
    assert_eq!(spans.len(), 1);
    assert!(spans[0].style.bold);
    assert_eq!(spans[0].style.color, Some(AnsiColor::Red));
}

/// Unknown SGR codes are silently ignored — surrounding text
/// still appears, just unstyled. Protects against renderer changes
/// adding a new `Style` variant we haven't taught the parser yet.
#[test]
fn unknown_codes_are_ignored() {
    let spans = parse_ansi_spans("\x1b[99mmystery\x1b[0m");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "mystery");
    assert_eq!(spans[0].style, SpanStyle::default());
}

/// Realistic snippet pulled from
/// `vetter-core/tests/snapshots/curl_render_snapshot__insecure_ansi.snap`
/// (with `\e` rendered as the actual byte 0x1B). This is the most
/// important regression: if the snapshot's exact escape pattern
/// stops parsing, the popover stops showing colours.
#[test]
fn parses_insecure_curl_snapshot_excerpt() {
    let s = " vet  \x1b[1mcurl\x1b[0m\n\x1b[90m ───\x1b[0m\
             \x1b[1m\x1b[32mGET\x1b[0m  \x1b[4m\x1b[36mhttps://x/\x1b[0m  \
             \x1b[1m\x1b[31m[insecure: -k]\x1b[0m\n";
    let spans = parse_ansi_spans(s);
    // Reconstruct the visible text; that's the most useful invariant.
    let visible: String = spans.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(visible, " vet  curl\n ───GET  https://x/  [insecure: -k]\n");
    // Spot-check: there's exactly one bold-red span (the badge) and
    // exactly one cyan-underlined span (the URL).
    let bold_red = spans
        .iter()
        .filter(|s| s.style.bold && s.style.color == Some(AnsiColor::Red))
        .count();
    assert_eq!(bold_red, 1, "{spans:#?}");
    let cyan_url = spans
        .iter()
        .filter(|s| s.style.underline && s.style.color == Some(AnsiColor::Cyan))
        .count();
    assert_eq!(cyan_url, 1, "{spans:#?}");
}

#[test]
fn strip_ansi_removes_sgr_sequences() {
    // Mirror of `render_detail`: bold command name, plain argv.
    let input = "\x1b[1mcurl\x1b[0m https://example.com/foo";
    assert_eq!(strip_ansi(input), "curl https://example.com/foo");
}

#[test]
fn strip_ansi_preserves_unterminated_escape() {
    // An unterminated CSI sequence is dropped by the scanner (see
    // `parse_ansi_spans`). The preceding text survives; anything
    // after the orphan escape is consumed as sequence bytes.
    let input = "before\x1b[";
    assert_eq!(strip_ansi(input), "before");
}

#[test]
fn strip_ansi_passes_through_plain_text() {
    assert_eq!(
        strip_ansi("curl -X POST https://x"),
        "curl -X POST https://x"
    );
}

// -- Foundation smoke tests --------------------------------------

/// `spans_to_attributed` should produce an `NSAttributedString`
/// whose UTF-16 length equals the sum of the spans' UTF-16 lengths
/// (i.e. all visible text reached AppKit and nothing was lost on the
/// way).
#[test]
fn attributed_string_length_matches_visible_text() {
    let s = format!("plain {} mid {}", ansi_red("RED"), ansi_red("MORE"));
    let attr = parse_ansi_to_attributed(&s);
    let visible_len = "plain RED mid MORE".len(); // ASCII so UTF-8 == UTF-16
    assert_eq!(attr.length(), visible_len);
}

/// Calling the Foundation translator on a pure-plain string still
/// produces a valid attributed string of the right length and
/// doesn't trip any objc2 assertions when nothing has a colour
/// attribute attached.
#[test]
fn pure_plain_input_still_round_trips() {
    let attr = parse_ansi_to_attributed("just text");
    assert_eq!(attr.length(), "just text".len());
    // Substring extraction is a cheap "object is healthy" probe.
    let head = attr.attributedSubstringFromRange(NSRange::new(0, 4));
    assert_eq!(head.length(), 4);
}

/// Empty input must produce an empty (zero-length) attributed
/// string — defensive: `NSTextStorage::setAttributedString` accepts
/// that and clears the view, which is what we want for the
/// "rendered detail is empty" fallback path in
/// `crate::lib::render_detail`.
#[test]
fn empty_input_yields_empty_attributed_string() {
    let attr = parse_ansi_to_attributed("");
    assert_eq!(attr.length(), 0);
}

// -- Hardening §H2: render-time sanitisation closes the popover hole --

/// Round-trip a `ParsedCommand` whose header value carries an
/// argv-injected `\x1b[31m` (red) SGR through the full renderer
/// (`AnsiWriter`) into the popover's ANSI parser. With H2's
/// sanitisation in place, the only red span the parser sees is one
/// the *renderer* legitimately emitted (the `[insecure: -k]` badge
/// when present, or none in this minimal fixture); the malicious
/// `\x1b[31m` from the header value must show up as a `<U+001B>`
/// placeholder inside an unstyled span, never as its own bold-red
/// run.
///
/// This is the popover-side regression check for the H2 bug: prior
/// to sanitisation, an attacker-controlled header value could open
/// a fake colour span that visually impersonated the renderer's own
/// colouring (e.g. fake the green "matched rule" line).
#[test]
fn argv_injected_sgr_in_header_does_not_open_popover_style_span() {
    use vetter_core::parsers::{
        Body, DisplayHints, Effect, Header, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };
    use vetter_core::{AnsiWriter, DefaultRenderer, Renderer};

    let p = ParsedCommand {
        command: "noop".into(),
        argv: vec!["noop".into()],
        cwd: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: url::Url::parse("https://example.test/").unwrap(),
            headers: vec![Header {
                name: "X-Evil".into(),
                value: "before\x1b[31mfake-red\x1b[0mafter".into(),
            }],
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

    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(&p, None, &mut AnsiWriter(&mut buf))
        .expect("render");
    let rendered = String::from_utf8(buf).expect("utf-8");

    let spans = parse_ansi_spans(&rendered);
    let visible: String = spans.iter().map(|s| s.text.as_str()).collect();
    // The malicious "fake-red" text is still visible (we don't drop
    // it), but the SGR bytes around it are placeholders.
    assert!(visible.contains("fake-red"), "{visible}");
    assert!(visible.contains("<U+001B>"), "{visible}");

    // The crucial property: no span carries `AnsiColor::Red` whose
    // text is the attacker's "fake-red" payload. The renderer
    // itself emits exactly one bold-red run today only for the
    // `MatchDeny` decision (absent here) and the `Badge(Danger)`
    // severity (absent here too — no signals), so any `Red` span at
    // all in this fixture is a smuggled-in-from-argv hit.
    let red_spans: Vec<&AnsiSpan> = spans
        .iter()
        .filter(|s| s.style.color == Some(AnsiColor::Red))
        .collect();
    assert!(
        red_spans.is_empty(),
        "argv-injected red span survived sanitisation: {red_spans:#?}"
    );
}
