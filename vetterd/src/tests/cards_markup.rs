//! Tests for [`crate::cards::markup`]. Layout convention from `AGENTS.md`.

use super::*;

#[test]
fn escapes_all_five_xml_entities() {
    assert_eq!(escape_markup(r#"<b>&"'"#), "&lt;b&gt;&amp;&quot;&apos;");
}

#[test]
fn leaves_ordinary_text_untouched() {
    // A URL is the common case and must survive byte-for-byte, or
    // every card would read wrong for the sake of a rare hostile one.
    let url = "https://api.example.test/v1/things";
    assert_eq!(escape_markup(url), url);
}

#[test]
fn a_span_tag_in_argv_derived_text_renders_inert() {
    // The attack this function exists to stop: a URL carrying markup
    // that would otherwise restyle the text a human reads before
    // approving. After escaping there is no `<` left to open a tag.
    let hostile = "https://evil.test/<span foreground='#00ff00'>looks-safe</span>";
    let escaped = escape_markup(hostile);
    assert!(
        !escaped.contains('<'),
        "no raw angle bracket may survive: {escaped}"
    );
    assert!(
        escaped.contains("&lt;span"),
        "the tag must appear as visible text: {escaped}"
    );
}

#[test]
fn escaping_is_idempotent_in_the_sense_that_matters() {
    // Not literally idempotent — `&` becomes `&amp;` becomes
    // `&amp;amp;` — so the invariant worth pinning is that we never
    // double-escape by accident, i.e. callers must escape exactly
    // once. This test documents that by showing the second pass does
    // change the string, which is why the escape happens at the
    // single point where text enters markup.
    let once = escape_markup("a&b");
    let twice = escape_markup(&once);
    assert_eq!(once, "a&amp;b");
    assert_ne!(once, twice, "escaping twice is a bug, not a no-op");
}

#[test]
fn ampersand_expands_before_the_entities_it_introduces() {
    // Ordering hazard: if `<` were rewritten to `&lt;` before `&` was
    // rewritten to `&amp;`, the ampersand we just introduced would be
    // escaped again and the output would read `&amp;lt;`. Escaping
    // per-character in one pass avoids that; this pins it.
    assert_eq!(escape_markup("<"), "&lt;");
    assert_eq!(escape_markup("&lt;"), "&amp;lt;");
}
