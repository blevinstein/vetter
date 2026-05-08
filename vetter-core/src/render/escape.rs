//! Render-side sanitisation for argv-controlled bytes.
//!
//! Renderer surfaces (`vet --explain` TTY output, the macOS popover,
//! the audit-log `rendered` field, the macOS notification banner)
//! display many strings that originate from the agent's argv: URLs,
//! header values, file paths, body content, etc. A malicious argv can
//! embed control sequences that visually rewrite or hide what the
//! human is approving:
//!
//! - ANSI escape sequences (`\x1b[2J` clear-screen, `\x1b[…H` cursor
//!   home, `\x1b[…m` to inject a fake style span past the popover's
//!   SGR scanner).
//! - Bare `CR` / `BS` / `BEL` to overwrite glyphs in a TTY.
//! - RTLO (U+202E) and the rest of the bidi-override / isolate set
//!   to flip text direction (the "trojan source" class).
//! - Zero-width chars (U+200B/C/D, U+FEFF) to hide path segments.
//! - C1 controls (U+0080-U+009F) — the 8-bit ANSI escape relatives.
//!
//! [`sanitize_for_display`] replaces every byte in those classes with
//! a printable substitute so the rendered text stays human-readable,
//! the attack surface is visible (instead of silently dropped), and
//! downstream layers (NSAttributedString, terminal emulators,
//! `popover_attr::parse_ansi_spans`) see only printable Unicode.
//!
//! For the three "ordinary whitespace" controls — LF, HT, CR — we
//! emit the matching glyph from the Unicode Control Pictures block
//! (`␊`, `␉`, `␍`). They're a single grapheme each and far less
//! visually noisy than `<U+000A>` clutter in a multi-line JSON body
//! while still being unmistakably *not* a real newline / tab. Every
//! other deny-listed codepoint (ESC, BEL, BS, DEL, C1, RTLO, ZWSP,
//! BOM, …) keeps the verbatim `<U+XXXX>` form so the exact byte stays
//! auditable.
//!
//! Trusted, renderer-owned strings (the rule line, indents, scope
//! labels, signal slugs, the writer's own SGR escape codes) MUST
//! bypass this helper — they're the only legitimate source of control
//! bytes in the rendered output. The convention is enforced by
//! convention at the call site, not by the type system.

use std::borrow::Cow;
use std::fmt::Write;

/// Replace every "dangerous" byte / codepoint with a printable
/// substitute. Returns [`Cow::Borrowed`] when `s` already contains
/// only safe characters (the common case — no allocation).
///
/// "Dangerous" set (per the H2 hardening item in `TODO.md`):
///
/// - C0 controls `0x00..=0x1F` (ESC, BEL, BS, CR, LF, TAB, FF, VT,
///   etc.). LF and TAB are *included* in the replace set: a fake
///   newline in an untrusted chunk can forge an entire renderer line
///   ("`   Match:        matched rule trusted-host`").
/// - DEL `0x7F`.
/// - C1 controls `U+0080..=U+009F`.
/// - Bidi overrides / embeddings / pop: `U+202A..=U+202E`.
/// - Bidi isolates / pop-isolate: `U+2066..=U+2069`.
/// - Bidi marks: `U+200E`, `U+200F`.
/// - Zero-width joiners / non-joiners: `U+200B..=U+200D`.
/// - BOM / ZWNBSP: `U+FEFF`.
///
/// LF / HT / CR are mapped to their compact Control Pictures glyphs
/// (`␊` U+240A, `␉` U+2409, `␍` U+240D) so that multi-line bodies and
/// tab-indented argv don't drown the surface in `<U+000A>` clutter,
/// while still surfacing the injection visibly. Everything else is
/// rendered as a verbatim 4-digit-min uppercase-hex form so the exact
/// codepoint remains auditable. Either way the downstream consumers
/// (terminal, AppKit, the popover ANSI parser) only see ordinary
/// printable characters.
pub fn sanitize_for_display(s: &str) -> Cow<'_, str> {
    if !s.chars().any(needs_escape) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push('\u{240A}'),
            '\t' => out.push('\u{2409}'),
            '\r' => out.push('\u{240D}'),
            _ if needs_escape(c) => {
                // Width is `max(4, hex digits of c)`; uppercase.
                // `write!` into a String never fails, so the unwrap
                // is infallible.
                let _ = write!(&mut out, "<U+{:04X}>", c as u32);
            }
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// True if `c` falls inside any of the deny-listed ranges documented
/// on [`sanitize_for_display`].
fn needs_escape(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        0x00..=0x1F            // C0 controls (incl. ESC, LF, TAB)
        | 0x7F                 // DEL
        | 0x0080..=0x009F      // C1 controls
        | 0x200B..=0x200F      // ZWSP/ZWNJ/ZWJ + LRM/RLM
        | 0x202A..=0x202E      // bidi embeddings + RLO/LRO
        | 0x2066..=0x2069      // bidi isolates + PDI
        | 0xFEFF               // BOM / ZWNBSP
    )
}

#[cfg(test)]
#[path = "../tests/render_escape.rs"]
mod tests;
