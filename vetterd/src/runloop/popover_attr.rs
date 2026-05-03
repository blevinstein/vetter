//! ANSI → `NSAttributedString` translator for the popover detail
//! body.
//!
//! [`crate::runloop::popover`] receives a pre-rendered §8.5 detail
//! string from [`crate::pending::PendingQueue`]; the daemon writes
//! that string with [`vetter_core::render::AnsiWriter`] so each span
//! is wrapped in CSI SGR escapes (the same bytes `vet --explain`
//! prints to a TTY). This module parses those escapes and translates
//! each span into matching `NSAttributedString` attributes —
//! foreground colour, bold/regular monospaced font, optional
//! underline — so the popover's `NSTextView` shows the same
//! risk/method/redaction colouring the CLI does.
//!
//! Two-stage pipeline keeps the parser unit-testable without booting
//! AppKit:
//!
//! 1. [`parse_ansi_spans`] reads `\x1b[<codes>m` runs and returns
//!    `Vec<AnsiSpan>` (plain text + accumulated style state). This
//!    half is pure Rust; no `objc2` types touched.
//! 2. [`spans_to_attributed`] walks the spans and stamps each onto a
//!    fresh `NSMutableAttributedString` using
//!    `addAttribute:value:range:`. Foundation methods only — no
//!    `MainThreadMarker` required even though the popover invokes
//!    this from the main queue.
//!
//! [`parse_ansi_to_attributed`] is the convenience wrapper the
//! popover calls.
//!
//! Recognised codes match `vetter_core::render::ansi_for` exactly:
//! `0` reset, `1` bold, `2` dim, `4` underline, FG `31/32/33/35/36`
//! and `90/94`. Unknown codes are ignored — the span text appears
//! unstyled rather than the parser bailing out, so a future widening
//! of the writer's palette degrades gracefully.

#![cfg(target_os = "macos")]

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_app_kit::{NSColor, NSFont};
use objc2_foundation::{
    ns_string, NSAttributedString, NSMutableAttributedString, NSNumber, NSRange, NSString,
};

/// Body font size used by the popover's detail text view.
pub const BODY_FONT_SIZE: f64 = 11.0;

/// Subset of ANSI foreground colours emitted by `ansi_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    Red,
    Green,
    Yellow,
    Magenta,
    Cyan,
    BrightBlack,
    BrightBlue,
}

/// Accumulated SGR state inside a run. `dim` is used both as a
/// loopback signal (cyan + dim → muted teal) and as a style modifier
/// when no colour is set (e.g. bright-black is treated as dim grey).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub color: Option<AnsiColor>,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
}

/// One contiguous run of plain text under a single style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// Parse `text` into spans. `\x1b[…m` sequences are stripped and
/// translated into [`SpanStyle`] state on the following run; any
/// other byte (including standalone `\x1b` not followed by `[`) is
/// retained verbatim so a renderer regression that emits unexpected
/// bytes still shows up in the popover instead of silently
/// corrupting output.
pub fn parse_ansi_spans(text: &str) -> Vec<AnsiSpan> {
    let mut spans: Vec<AnsiSpan> = Vec::new();
    let mut style = SpanStyle::default();
    let mut buf = String::new();
    let mut iter = text.chars().peekable();

    while let Some(c) = iter.next() {
        if c == '\x1b' && iter.peek() == Some(&'[') {
            iter.next(); // consume '['
                         // Flush whatever text we've accumulated under the
                         // *previous* style before the new SGR takes effect.
            if !buf.is_empty() {
                spans.push(AnsiSpan {
                    text: std::mem::take(&mut buf),
                    style,
                });
            }
            // Read until 'm', collecting digits separated by ';'.
            // Any non-digit, non-';' byte aborts the SGR scan and
            // we drop the (malformed) sequence; this matches what
            // anstyle's reset emitter could conceivably emit if
            // future combinators are added.
            let mut codes_str = String::new();
            let mut terminated = false;
            for c2 in iter.by_ref() {
                if c2 == 'm' {
                    terminated = true;
                    break;
                }
                codes_str.push(c2);
            }
            if !terminated {
                continue;
            }
            for code in codes_str.split(';') {
                if code.is_empty() {
                    // CSI `m` with no params is implicit `0` (reset).
                    apply_sgr(&mut style, 0);
                } else if let Ok(n) = code.parse::<u32>() {
                    apply_sgr(&mut style, n);
                }
            }
            continue;
        }
        buf.push(c);
    }
    if !buf.is_empty() {
        spans.push(AnsiSpan { text: buf, style });
    }
    spans
}

fn apply_sgr(style: &mut SpanStyle, code: u32) {
    match code {
        0 => *style = SpanStyle::default(),
        1 => style.bold = true,
        2 => style.dim = true,
        4 => style.underline = true,
        // Foreground colours used by `vetter_core::render::ansi_for`.
        // Anything outside this list is intentionally ignored — we
        // don't want a future writer change to crash the popover.
        31 => style.color = Some(AnsiColor::Red),
        32 => style.color = Some(AnsiColor::Green),
        33 => style.color = Some(AnsiColor::Yellow),
        35 => style.color = Some(AnsiColor::Magenta),
        36 => style.color = Some(AnsiColor::Cyan),
        90 => style.color = Some(AnsiColor::BrightBlack),
        94 => style.color = Some(AnsiColor::BrightBlue),
        _ => {}
    }
}

/// Convert a span list into an `NSMutableAttributedString`. Each
/// span is appended as plain text first, then attributes are stamped
/// over the corresponding `NSRange` (in UTF-16 code units, which is
/// what `NSString::length` returns and what AppKit text drawing
/// expects).
pub fn spans_to_attributed(spans: &[AnsiSpan]) -> Retained<NSMutableAttributedString> {
    let acc = NSMutableAttributedString::new();
    for span in spans {
        let nstr = NSString::from_str(&span.text);
        let span_len = nstr.length();
        let start = acc.length();
        let plain = NSAttributedString::initWithString(NSAttributedString::alloc(), &nstr);
        acc.appendAttributedString(&plain);
        if span_len == 0 {
            continue;
        }
        let range = NSRange::new(start, span_len);
        apply_attributes(&acc, range, span.style);
    }
    acc
}

/// Convenience wrapper. Equivalent to
/// `spans_to_attributed(&parse_ansi_spans(text))`.
pub fn parse_ansi_to_attributed(text: &str) -> Retained<NSMutableAttributedString> {
    spans_to_attributed(&parse_ansi_spans(text))
}

fn apply_attributes(acc: &NSMutableAttributedString, range: NSRange, style: SpanStyle) {
    // Always stamp a foreground colour, even on un-styled spans —
    // see `ns_color_for` for the rationale (TL;DR:
    // `setAttributedString:` wipes `NSTextView`'s typing colour, so
    // ranges with no `NSForegroundColorAttributeName` fall back to
    // AppKit's hard-coded black instead of the popover's
    // appearance-adaptive `textColor`, which made plain command text
    // unreadable on the dark popover surface).
    let color = ns_color_for(style);
    // SAFETY: `ns_string!("NSColor")` is the literal Cocoa
    // attribute key (`NSForegroundColorAttributeName`'s string
    // value), `color` is an `NSColor` (matches the documented
    // value type), and `range` was just computed from
    // `acc.length()` + `nstr.length()` so it lies fully inside
    // the attributed string.
    unsafe {
        acc.addAttribute_value_range(ns_string!("NSColor"), &color, range);
    }
    let font = ns_font_for(style);
    // SAFETY: `ns_string!("NSFont")` is the documented Cocoa
    // attribute-name string for `NSFontAttributeName`; `font` is an
    // `NSFont` of the documented value type; `range` is in-bounds
    // (see above).
    unsafe {
        acc.addAttribute_value_range(ns_string!("NSFont"), &font, range);
    }
    if style.underline {
        // `NSUnderlineStyleSingle` == 1. Wrapping it in an
        // `NSNumber` is the standard way to attach an integer
        // attribute value.
        let one = NSNumber::new_i32(1);
        // SAFETY: `ns_string!("NSUnderline")` is the documented
        // attribute key for `NSUnderlineStyleAttributeName`; the
        // value type is `NSNumber` per Apple's docs; range is
        // in-bounds.
        unsafe {
            acc.addAttribute_value_range(ns_string!("NSUnderline"), &one, range);
        }
    }
}

/// Map a `SpanStyle` to the `NSColor` we want stamped on its run.
///
/// Returns *some* colour for every input — un-styled runs (no ANSI
/// colour code seen) fall through to `NSColor::textColor()`, the
/// appearance-adaptive default that resolves to a near-white on
/// the popover's pinned Dark Aqua surface and to black under Light
/// Aqua. Stamping it explicitly (rather than leaving the run with
/// no `NSForegroundColorAttributeName`) is what makes the plain
/// command text readable: `NSTextView`'s typing-colour default
/// gets reset by `storage.setAttributedString:`, so ranges without
/// an explicit colour fall back to AppKit's hard-coded black —
/// which produced the "raw text is unreadable, black on dark"
/// regression after we forced the popover into Dark Aqua.
fn ns_color_for(style: SpanStyle) -> Retained<NSColor> {
    let Some(color) = style.color else {
        return NSColor::textColor();
    };
    match (color, style.dim) {
        (AnsiColor::Red, _) => NSColor::systemRedColor(),
        (AnsiColor::Green, _) => NSColor::systemGreenColor(),
        (AnsiColor::Yellow, _) => NSColor::systemYellowColor(),
        (AnsiColor::Magenta, _) => NSColor::systemPurpleColor(),
        // Loopback URLs come through as `cyan + dim`; route them to
        // a muted secondary colour so they read as "less alarming"
        // than a real outgoing URL.
        (AnsiColor::Cyan, true) => NSColor::secondaryLabelColor(),
        (AnsiColor::Cyan, false) => NSColor::systemTealColor(),
        // Bright-black in the renderer is "rule line" / dim metadata.
        // Map both to the system secondary-label colour so light/dark
        // mode adaptation comes for free.
        (AnsiColor::BrightBlack, _) => NSColor::secondaryLabelColor(),
        (AnsiColor::BrightBlue, _) => NSColor::systemBlueColor(),
    }
}

fn ns_font_for(style: SpanStyle) -> Retained<NSFont> {
    if style.bold {
        // The body view uses a fixed-pitch font; for bold we stick
        // with the *user* fixed-pitch face but render via
        // `boldSystemFontOfSize` so the bold weight is honoured even
        // when the user's monospaced face has no bold variant.
        // (`monospacedSystemFontOfSize:weight:` would be cleaner but
        // requires reaching for the `NSFontWeight` extern static —
        // not worth the unsafe block for the same visual result.)
        NSFont::boldSystemFontOfSize(BODY_FONT_SIZE)
    } else {
        NSFont::userFixedPitchFontOfSize(BODY_FONT_SIZE)
            .unwrap_or_else(|| NSFont::systemFontOfSize(BODY_FONT_SIZE))
    }
}

#[cfg(test)]
#[path = "../tests/popover_attr.rs"]
mod tests;
