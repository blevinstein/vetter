//! Tests for [`crate::runloop::popover_attr`].
//!
//! Only the Foundation half lives here now. The parser
//! ([`crate::cards::spans::parse_ansi_spans`]) moved into the shared
//! card layer along with its assertions — see
//! `crate::tests::cards_spans`, which runs on every target rather
//! than just macOS.
//!
//! [`spans_to_attributed`] gets a smoke test that asserts on the
//! resulting `NSAttributedString`'s `length()` — a proxy for "every
//! span made it across without tripping AppKit". We avoid asserting
//! on `NSColor` / `NSFont` equality directly because objc2 doesn't
//! expose stable equality for those (and Apple doesn't promise
//! factory-method singletons either).

#![cfg(target_os = "macos")]

use objc2_foundation::NSRange;

use super::*;

fn ansi_red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
}

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
