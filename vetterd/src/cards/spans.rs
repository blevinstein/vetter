//! ANSI SGR → styled spans, for the §8.5 detail body.
//!
//! The daemon pre-renders each request's detail string with
//! [`vetter_core::render::AnsiWriter`], so every styled run arrives
//! wrapped in CSI SGR escapes — the same bytes `vet --explain`
//! prints to a TTY. Approval surfaces need that styling back as
//! structured data before they can paint it: macOS stamps
//! `NSAttributedString` attributes over the runs, GTK will emit
//! Pango markup. Parsing the escapes is identical work either way,
//! so it happens once, here.
//!
//! Recognised codes match [`vetter_core::render::ansi_for`] exactly:
//! `0` reset, `1` bold, `2` dim, `4` underline, FG `31/32/33/35/36`
//! and `90/94`. Unknown codes are ignored — the span text appears
//! unstyled rather than the parser bailing out, so a future widening
//! of the writer's palette degrades gracefully rather than blanking
//! the body.
//!
//! ## On the type names
//!
//! [`AnsiColor`] names the *source encoding*, not a toolkit: it says
//! "the writer asked for its green", leaving each platform to decide
//! which green that is (`NSColor::systemGreenColor()` on macOS, a
//! theme colour on GTK). That is the neutral layer both surfaces map
//! from, which is why the parser stops here rather than resolving to
//! anything paintable.

/// Subset of ANSI foreground colours emitted by
/// [`vetter_core::render::ansi_for`].
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
/// bytes still shows up in the UI instead of silently corrupting
/// output.
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
        // don't want a future writer change to crash the UI.
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

/// Strip SGR escapes from `text`, returning just the printable
/// characters. Shares the scanner with [`parse_ansi_spans`] so any
/// bytes that survive there (malformed / unterminated sequences)
/// survive here too — "what the user sees in the card" should
/// round-trip cleanly to what lands on the clipboard when they hit
/// the copy button. Used by the copy-to-clipboard affordance next
/// to the "Show raw" disclosure.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for span in parse_ansi_spans(text) {
        out.push_str(&span.text);
    }
    out
}

#[cfg(test)]
#[path = "../tests/cards_spans.rs"]
mod tests;
