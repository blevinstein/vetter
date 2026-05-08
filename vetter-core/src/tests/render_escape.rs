//! Tests for [`crate::render::escape`]. Layout convention is described
//! in `AGENTS.md`.

use super::*;
use std::borrow::Cow;

#[test]
fn plain_ascii_is_borrowed_unchanged() {
    let s = "GET https://api.example.com/users/42";
    let out = sanitize_for_display(s);
    assert!(matches!(out, Cow::Borrowed(_)));
    assert_eq!(&*out, s);
}

#[test]
fn ordinary_unicode_is_borrowed_unchanged() {
    // Mixed-script printable text — Han + Cyrillic + emoji + accents
    // — must round-trip unchanged. Only the explicit deny-listed
    // ranges are considered dangerous.
    let s = "café — 世界 — Привет — 🦀";
    let out = sanitize_for_display(s);
    assert!(matches!(out, Cow::Borrowed(_)), "got `{out}`");
    assert_eq!(&*out, s);
}

#[test]
fn esc_byte_is_replaced() {
    let s = "before\x1b[2J\x1b[Hafter";
    let out = sanitize_for_display(s);
    assert!(matches!(out, Cow::Owned(_)));
    assert!(!out.contains('\x1b'), "raw ESC survived: `{out}`");
    // Each ESC + each control char in the sequence gets escaped.
    assert!(
        out.contains("<U+001B>"),
        "missing ESC placeholder in `{out}`"
    );
    assert!(out.starts_with("before"));
    assert!(out.ends_with("after"));
}

#[test]
fn newline_and_tab_are_replaced_in_untrusted_text() {
    // Newlines in untrusted chunks could forge an entire renderer
    // line, so they're explicitly inside the deny set — but they map
    // to the compact Control Pictures glyphs rather than the noisy
    // `<U+XXXX>` form.
    let out = sanitize_for_display("line1\nline2\tafter");
    assert!(matches!(out, Cow::Owned(_)));
    assert!(!out.contains('\n'));
    assert!(!out.contains('\t'));
    assert!(out.contains('\u{240A}'));
    assert!(out.contains('\u{2409}'));
}

#[test]
fn cr_bs_bel_are_replaced() {
    // CR shares the whitespace carve-out (compact glyph); BS and BEL
    // stay in the verbatim `<U+XXXX>` form.
    for (raw, label) in [
        ("a\rb", "\u{240D}"),
        ("a\x08b", "<U+0008>"),
        ("a\x07b", "<U+0007>"),
    ] {
        let out = sanitize_for_display(raw);
        assert!(out.contains(label), "expected `{label}` in `{out}`");
        assert!(!out.contains('\r'));
    }
}

#[test]
fn del_and_c1_controls_are_replaced() {
    let out = sanitize_for_display("a\x7fb");
    assert!(out.contains("<U+007F>"), "got `{out}`");

    // C1 controls (0x80..=0x9F) appear as multi-byte UTF-8 in `&str`
    // but are still in our deny range.
    let s = "a\u{0085}b\u{009F}c";
    let out = sanitize_for_display(s);
    assert!(out.contains("<U+0085>"), "got `{out}`");
    assert!(out.contains("<U+009F>"), "got `{out}`");
    assert!(!out.contains('\u{0085}'));
}

#[test]
fn rtlo_and_bidi_overrides_are_replaced() {
    // The "trojan source" set: U+202A..=U+202E.
    for c in ['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}'] {
        let s = format!("path/{c}fake");
        let out = sanitize_for_display(&s);
        assert!(!out.contains(c), "raw bidi `{c:?}` survived: `{out}`");
        assert!(
            out.contains(&format!("<U+{:04X}>", c as u32)),
            "missing placeholder for `{c:?}` in `{out}`"
        );
    }
}

#[test]
fn bidi_isolates_and_marks_are_replaced() {
    for c in [
        '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{200E}', '\u{200F}',
    ] {
        let s = format!("a{c}b");
        let out = sanitize_for_display(&s);
        assert!(!out.contains(c), "raw `{c:?}` survived: `{out}`");
    }
}

#[test]
fn zero_width_chars_are_replaced() {
    for c in ['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'] {
        let s = format!("a{c}b");
        let out = sanitize_for_display(&s);
        assert!(!out.contains(c), "raw zero-width `{c:?}` survived: `{out}`");
    }
}

#[test]
fn esc_in_longer_string_returns_owned() {
    let s = "https://example.com/path?q=\x1b[31mred\x1b[0m";
    let out = sanitize_for_display(s);
    assert!(matches!(out, Cow::Owned(_)));
    assert!(!out.contains('\x1b'));
    assert!(out.contains("https://example.com/path?q="));
    assert!(out.contains("red"));
}

#[test]
fn sanitize_is_idempotent() {
    let dirty = "line1\n\x1b[2Jpath/\u{202E}rev\u{200B}";
    let once = sanitize_for_display(dirty).into_owned();
    let twice = sanitize_for_display(&once).into_owned();
    assert_eq!(once, twice);
    // Once sanitised, the result must be borrow-clean.
    assert!(matches!(sanitize_for_display(&once), Cow::Borrowed(_)));
}

#[test]
fn empty_string_is_passed_through() {
    let out = sanitize_for_display("");
    assert!(matches!(out, Cow::Borrowed(_)));
    assert_eq!(&*out, "");
}
