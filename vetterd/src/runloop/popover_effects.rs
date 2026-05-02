//! Per-effect native rows for the approver popover.
//!
//! [`build_effect_views`] walks `parsed.effects` and produces one
//! `NSView` per item, ready to be appended to the card stack:
//!
//! - `HttpRequest` → headers list, body summary, auth row.
//!   The request line is *not* repeated — it's already in the URL
//!   row at the top of the card.
//! - `FileRead` / `FileWrite` → SF-Symbol glyph + monospaced path.
//! - `ProcessSpawn` → terminal glyph + monospaced command.
//! - `CredentialUse` / `Network` → skipped (the §8.5 layout doesn't
//!   render them today either; "Show raw" still surfaces them).
//!
//! The whole module is presentational — it never mutates the
//! parsed command, never branches on `command`, and never reads
//! anything outside `vetter_core::parsers` types + the redaction
//! helpers in `vetter_core::render`. That keeps the renderer →
//! popover symmetry called out in `plans/ApprovalUI.md "Goals"`.
//!
//! See [plans/ApprovalUI.md "Effect rows"](../../../plans/ApprovalUI.md)
//! for the full token table.

#![cfg(target_os = "macos")]

use objc2::rc::Retained;
use objc2_app_kit::{
    NSColor, NSFont, NSFontWeightSemibold, NSImage, NSImageView, NSStackView,
    NSStackViewDistribution, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use vetter_core::render::{is_secret_header, redact_value};
use vetter_core::{Auth, Body, Effect, FileRead, FileWrite, Header, ParsedCommand, ProcessSpawn};

/// Body / monospaced-label font size. Picked to read at the same
/// optical weight as the URL row's tokens.
const ROW_FONT_SIZE: f64 = 12.0;
/// Section header (e.g. "headers", "body", "auth") font size.
const SECTION_FONT_SIZE: f64 = 11.0;

/// Glyph point size for SF Symbols. 13pt aligns roughly with the
/// monospaced path label baseline.
const GLYPH_SIZE: f64 = 13.0;

/// Walk `parsed.effects` and produce a flat `Vec<NSView>` of
/// per-effect rows in source order. Empty effects (today: only
/// `Body::None` HTTP bodies) are silently dropped so the resulting
/// stack has no blank slots.
pub fn build_effect_views(parsed: &ParsedCommand, mtm: MainThreadMarker) -> Vec<Retained<NSView>> {
    let mut out: Vec<Retained<NSView>> = Vec::new();
    for eff in &parsed.effects {
        match eff {
            Effect::HttpRequest(req) => {
                if let Some(view) = build_headers_section(&req.headers, mtm) {
                    out.push(view);
                }
                if let Some(view) = build_body_section(&req.body, mtm) {
                    out.push(view);
                }
                if let Some(view) = build_auth_row(req.auth.as_ref(), mtm) {
                    out.push(view);
                }
            }
            Effect::FileRead(fr) => out.push(build_file_read_row(fr, mtm)),
            Effect::FileWrite(fw) => out.push(build_file_write_row(fw, mtm)),
            Effect::ProcessSpawn(ps) => out.push(build_process_row(ps, mtm)),
            Effect::CredentialUse(_) | Effect::Network(_) => {}
        }
    }
    out
}

/// Build the headers section for an HttpRequest. Returns `None`
/// when `headers` is empty so the caller doesn't insert a blank
/// "headers" label.
fn build_headers_section(headers: &[Header], mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    if headers.is_empty() {
        return None;
    }
    let stack = vertical_section("headers", mtm);
    for h in headers {
        stack.addArrangedSubview(&build_header_row(h, mtm));
    }
    Some(stack.into_super())
}

/// Single header row: `name: value`, with redaction for secret
/// headers. Name is bold blue (matching the CLI `HeaderName` style);
/// value is monospaced regular, or red `••••<last4>` plus a dim
/// "len N" suffix when redacted.
fn build_header_row(h: &Header, mtm: MainThreadMarker) -> Retained<NSView> {
    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(4.0);

    let name = NSTextField::labelWithString(&NSString::from_str(&format!("{}:", h.name)), mtm);
    name.setFont(Some(&NSFont::boldSystemFontOfSize(ROW_FONT_SIZE)));
    name.setTextColor(Some(&NSColor::systemBlueColor()));
    row.addArrangedSubview(&name);

    if is_secret_header(&h.name) {
        let redacted = redact_value(&h.value);
        let value = NSTextField::labelWithString(&NSString::from_str(&redacted), mtm);
        value.setFont(Some(&monospaced(ROW_FONT_SIZE)));
        value.setTextColor(Some(&NSColor::systemRedColor()));
        row.addArrangedSubview(&value);

        let suffix = NSTextField::labelWithString(
            &NSString::from_str(&format!("← redacted, len {}", h.value.len())),
            mtm,
        );
        suffix.setFont(Some(&NSFont::systemFontOfSize(ROW_FONT_SIZE - 1.0)));
        suffix.setTextColor(Some(&NSColor::secondaryLabelColor()));
        row.addArrangedSubview(&suffix);
    } else {
        let value = NSTextField::labelWithString(&NSString::from_str(&h.value), mtm);
        value.setFont(Some(&monospaced(ROW_FONT_SIZE)));
        row.addArrangedSubview(&value);
    }

    let spacer = NSView::new(mtm);
    row.addArrangedSubview(&spacer);
    row.into_super()
}

/// Build the body section. Returns `None` for `Body::None` so we
/// don't render a stub "body" label for bodyless GETs.
fn build_body_section(body: &Body, mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    let (meta, content): (String, Option<Retained<NSView>>) = match body {
        Body::None => return None,
        Body::Inline { bytes } => (
            format!("inline, {} B", bytes.len()),
            Some(inline_body_content(bytes, mtm)),
        ),
        Body::FromFile { path } => (
            "from file".to_string(),
            Some(file_glyph_row("doc.text", &path.display().to_string(), mtm)),
        ),
        Body::FromStdin { digest, len } => (
            format!("from stdin, {len} B, sha256 {}", digest.as_str()),
            None,
        ),
        Body::Form { fields } => {
            let stack = NSStackView::new(mtm);
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setSpacing(2.0);
            stack.setDistribution(NSStackViewDistribution::Fill);
            for f in fields {
                let pair = NSTextField::labelWithString(
                    &NSString::from_str(&format!("{}={}", f.name, f.value)),
                    mtm,
                );
                pair.setFont(Some(&monospaced(ROW_FONT_SIZE)));
                stack.addArrangedSubview(&pair);
            }
            (
                format!("x-www-form-urlencoded, {} fields", fields.len()),
                Some(stack.into_super()),
            )
        }
    };

    let stack = vertical_section("body", mtm);
    let meta_label = NSTextField::labelWithString(&NSString::from_str(&meta), mtm);
    meta_label.setFont(Some(&NSFont::systemFontOfSize(ROW_FONT_SIZE - 1.0)));
    meta_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    stack.addArrangedSubview(&meta_label);
    if let Some(view) = content {
        stack.addArrangedSubview(&view);
    }
    Some(stack.into_super())
}

/// Render the inline bytes either as a UTF-8 string (when valid)
/// or a short hex dump (when not). Wrapped in a label rather than
/// a scrollable text view because bodies in v1 are usually short
/// (form posts, JSON deltas); long bodies are still reachable via
/// "Show raw" and the future scroll-on-hover treatment is tracked
/// in the design doc.
fn inline_body_content(bytes: &[u8], mtm: MainThreadMarker) -> Retained<NSView> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let max = 64.min(bytes.len());
            let hex: String = bytes[..max]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            if max < bytes.len() {
                format!("{hex} … ({} more bytes)", bytes.len() - max)
            } else {
                hex
            }
        }
    };
    let label = NSTextField::labelWithString(&NSString::from_str(&text), mtm);
    label.setFont(Some(&monospaced(ROW_FONT_SIZE)));
    label.into_super().into_super()
}

/// Auth row. Returns `None` when no auth is set so we don't paint
/// an "auth: (none)" line for unauthenticated requests.
///
/// Colour rule: the auth row is **green** when the credential value
/// has been redacted out of the UI (the safe state — the agent has
/// auth and we kept the actual secret off-screen). Unredacted
/// values (today: only `Auth::Netrc`, where the credential never
/// reaches us in the first place) render in the secondary label
/// colour. We deliberately do *not* paint this row red for redacted
/// credentials: having auth on a request is generally a good sign,
/// and a red row was reading as "secret leaked" when the opposite
/// is true.
fn build_auth_row(auth: Option<&Auth>, mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    let auth = auth?;
    let (label, redacted) = match auth {
        Auth::Basic {
            user,
            password_redacted,
        } => (
            format!("Basic user={user} password=••••"),
            *password_redacted,
        ),
        Auth::Bearer { token_redacted } => ("Bearer ••••".to_string(), *token_redacted),
        Auth::Header { name } => (format!("{name}: ••••"), true),
        Auth::Netrc => ("from .netrc".to_string(), false),
    };
    let stack = vertical_section("auth", mtm);
    let value = NSTextField::labelWithString(&NSString::from_str(&label), mtm);
    value.setFont(Some(&monospaced(ROW_FONT_SIZE)));
    if redacted {
        value.setTextColor(Some(&NSColor::systemGreenColor()));
    } else {
        value.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    stack.addArrangedSubview(&value);
    Some(stack.into_super())
}

fn build_file_read_row(fr: &FileRead, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = vertical_section("read", mtm);
    stack.addArrangedSubview(&file_glyph_row(
        "doc.text",
        &fr.path.display().to_string(),
        mtm,
    ));
    stack.into_super()
}

fn build_file_write_row(fw: &FileWrite, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = vertical_section("write", mtm);
    stack.addArrangedSubview(&file_glyph_row(
        "square.and.pencil",
        &fw.path.display().to_string(),
        mtm,
    ));
    stack.into_super()
}

fn build_process_row(ps: &ProcessSpawn, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = vertical_section("spawn", mtm);
    stack.addArrangedSubview(&file_glyph_row("terminal", &ps.command, mtm));
    stack.into_super()
}

/// Build a single horizontal row of `[glyph]  monospaced-text`.
/// Used by every "this is a file/process" row so they read
/// uniformly. The glyph falls back silently to nothing when the
/// running OS doesn't ship the requested SF Symbol (older macOS,
/// future renames).
fn file_glyph_row(symbol: &str, text: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(6.0);
    row.setDistribution(NSStackViewDistribution::Fill);

    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        None,
    ) {
        let view = NSImageView::imageViewWithImage(&image, mtm);
        view.setFrameSize(NSSize::new(GLYPH_SIZE, GLYPH_SIZE));
        let _ = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(GLYPH_SIZE, GLYPH_SIZE));
        row.addArrangedSubview(&view);
    }

    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&monospaced(ROW_FONT_SIZE)));
    row.addArrangedSubview(&label);

    let spacer = NSView::new(mtm);
    row.addArrangedSubview(&spacer);
    row.into_super()
}

/// Make a vertical `NSStackView` titled with a small dim section
/// label (e.g. "headers", "body"). Returns the inner stack — the
/// title is already added as the first arranged subview, so the
/// caller just appends rows.
fn vertical_section(title: &str, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setSpacing(2.0);
    stack.setDistribution(NSStackViewDistribution::Fill);

    let title_label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
    let semibold = unsafe { NSFontWeightSemibold };
    title_label.setFont(Some(&NSFont::systemFontOfSize_weight(
        SECTION_FONT_SIZE,
        semibold,
    )));
    title_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    stack.addArrangedSubview(&title_label);
    stack
}

fn monospaced(size: f64) -> Retained<NSFont> {
    NSFont::userFixedPitchFontOfSize(size).unwrap_or_else(|| NSFont::systemFontOfSize(size))
}

#[cfg(test)]
#[path = "../tests/popover_effects.rs"]
mod tests;
