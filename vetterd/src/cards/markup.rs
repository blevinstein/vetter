//! XML-entity escaping for the two markup-bearing approval surfaces.
//!
//! Two of our surfaces render *markup*, not plain text, and both use
//! the same XML-ish syntax:
//!
//! - freedesktop notifications, when the server advertises the
//!   `body-markup` capability ([`crate::notifier::linux`]);
//! - Pango, which is how a GTK label paints per-run colour and weight
//!   ([`crate::runloop::linux`]).
//!
//! Everything they paint is argv-derived. An unescaped `<span
//! foreground='…'>` in a URL or a header name would therefore be
//! *parsed as markup* on the exact surface a human is reading to
//! decide whether to approve the command — a request could restyle or
//! hide the text describing itself. That is the same class of hole as
//! the header-injection defences in the renderer, and it applies
//! identically to both surfaces, so the escape lives here rather than
//! being written twice.
//!
//! This is **not** a substitute for
//! [`vetter_core::render::sanitize_for_display`], which strips RTLO,
//! zero-width and control bytes. The two compose: sanitise first so
//! the *characters* cannot lie about their order, escape second so the
//! *markup* cannot lie about its structure. Neither alone is enough.

/// Escape the five entities XML (and therefore both Pango markup and
/// the freedesktop `body-markup` subset) treats specially.
///
/// `'` and `"` are escaped even though they are only strictly
/// significant inside attribute values: we never interpolate
/// untrusted text into an attribute, but escaping them costs nothing
/// and removes the question from any future caller that does.
///
/// Apply this **only** when the text is going somewhere markup is
/// parsed. A notification server without `body-markup`, or a GTK
/// label set with `set_text` rather than `set_markup`, renders the
/// escapes literally — the user would read `&amp;` where they should
/// read `&`.
pub fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/cards_markup.rs"]
mod tests;
