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

use std::path::Path;

use objc2::rc::Retained;
use objc2_app_kit::{
    NSButton, NSColor, NSFont, NSFontWeightSemibold, NSImage, NSImageView, NSStackView,
    NSStackViewDistribution, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use vetter_core::render::sanitize_for_display;
use vetter_core::{Auth, Body, Effect, FileRead, FileWrite, Header, ParsedCommand, ProcessSpawn};

use crate::cards::effects as card_effects;

/// Body / monospaced-label font size. Picked to read at the same
/// optical weight as the URL row's tokens.
const ROW_FONT_SIZE: f64 = 12.0;
/// Section header (e.g. "headers", "body", "auth") font size.
const SECTION_FONT_SIZE: f64 = 11.0;

/// Glyph point size for SF Symbols. 13pt aligns roughly with the
/// monospaced path label baseline.
const GLYPH_SIZE: f64 = 13.0;

/// Factory the popover supplies so this presentation-only module
/// can attach an "Open file" button next to FileRead /
/// `Body::FromFile` rows without taking a hard dependency on
/// [`super::popover::PopoverController`]. Returning `None` means
/// "skip the button on this row" — the popover side returns `None`
/// when `path.exists()` is false so writes-to-be-created and
/// dangling references don't sprout a useless button. Tests pass a
/// `|_, _| None` stub for "no button" coverage and a small factory
/// returning `Some(NSButton::new(mtm))` to exercise the wiring
/// without needing a real on-disk file.
pub type FileButtonFactory<'a> = dyn Fn(&Path, MainThreadMarker) -> Option<Retained<NSButton>> + 'a;

/// Per-effect views split by visibility class so the popover can
/// keep the Recent stack compact without hiding the Phase 5.1
/// "Open file" button behind a disclosure.
///
/// - `file_inputs`: file-input rows — `Effect::FileRead` plus the
///   `Body::FromFile` branch of `build_body_section`. Always rendered
///   inline on both pending and resolved cards so the Open button is
///   one click away after a request moves into "Recent".
/// - `others`: everything else — headers, non-file body, auth,
///   `FileWrite`, `ProcessSpawn`. Rendered inline on pending cards
///   (the user is deciding now and wants to see them); on resolved
///   cards they live behind the "▸ Details" disclosure.
///
/// Within each bucket, items are emitted in source order. Across
/// buckets, the popover paints `file_inputs` first (visible), then
/// `others` (possibly hidden). Pending cards show both inline so
/// the global ordering of headers / body / auth on a single card
/// matches the §8.5 renderer.
pub struct EffectViews {
    pub file_inputs: Vec<Retained<NSView>>,
    pub others: Vec<Retained<NSView>>,
}

// `total` and `is_empty` are convenience helpers used by the
// row-count assertions in `crate::tests::popover_effects`. The
// popover itself reaches into `file_inputs` / `others` directly
// because pending and resolved cards lay them out differently
// (see `PopoverController::build_card`), so a single "is everything
// empty?" predicate isn't enough on the production path. Marking
// the impl `#[allow(dead_code)]` keeps clippy quiet on the lib
// build (where `#[cfg(test)]` test modules are absent) without
// hiding either method from the test binary.
#[allow(dead_code)]
impl EffectViews {
    /// Total number of per-effect rows across both buckets.
    pub fn total(&self) -> usize {
        self.file_inputs.len() + self.others.len()
    }

    /// True when both buckets are empty — the GET-with-no-headers
    /// case, after CredentialUse / Network filtering, etc.
    pub fn is_empty(&self) -> bool {
        self.file_inputs.is_empty() && self.others.is_empty()
    }
}

/// Walk `parsed.effects` and produce a per-effect view set, split
/// into file-input rows (always visible on both card kinds) and
/// "other" rows (possibly hidden behind a disclosure on resolved
/// cards). Empty effects (today: only `Body::None` HTTP bodies)
/// are silently dropped so neither bucket has blank slots.
///
/// `file_button_factory` is consulted for every file-input row
/// (`FileRead` and the `Body::FromFile` branch of `build_body_section`).
/// File outputs (`FileWrite`) and `ProcessSpawn` deliberately don't
/// get the button this round — write paths may not exist yet, and
/// "Reveal in Finder" is a separate UX call left for follow-up.
///
/// De-duplication: the curl parser intentionally emits **both**
/// `Body::FromFile { path }` (for `-d @file`) **and** a separate
/// `Effect::FileRead { path }` so the matcher / audit log can
/// reason about the file read independently of the HTTP body.
/// That double-bookkeeping is invisible in the §8.5 renderer (one
/// "body" line, one "files" line) but the popover would render two
/// identical rows — body row + read row, same path — without help.
/// Any FileRead whose path also appears as a `Body::FromFile` in
/// this parsed command is skipped on the read row; the body row
/// already exposes the path and the Open button. We keep the
/// FileRead in the underlying effect list (the matcher still sees
/// it) — only the visible row collapses.
pub fn build_effect_views(
    parsed: &ParsedCommand,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> EffectViews {
    let body_file_paths = card_effects::collect_body_file_paths(parsed);
    let mut out = EffectViews {
        file_inputs: Vec::new(),
        others: Vec::new(),
    };
    for eff in &parsed.effects {
        match eff {
            Effect::HttpRequest(req) => {
                if let Some(view) = build_headers_section(&req.headers, mtm) {
                    out.others.push(view);
                }
                if let Some(view) = build_body_section(&req.body, mtm, file_button_factory) {
                    // `Body::FromFile` is the only file-input
                    // variant of the body section. Other body
                    // shapes (Inline / Form) live in `others` so
                    // they fall behind the resolved-card disclosure
                    // with the rest of the request metadata.
                    if matches!(req.body, Body::FromFile { .. }) {
                        out.file_inputs.push(view);
                    } else {
                        out.others.push(view);
                    }
                }
                if let Some(view) = build_auth_row(req.auth.as_ref(), mtm) {
                    out.others.push(view);
                }
            }
            Effect::FileRead(fr) => {
                if body_file_paths.contains(&fr.path) {
                    // Path already surfaced by the matching
                    // `Body::FromFile` row above — skip the duplicate.
                    continue;
                }
                out.file_inputs
                    .push(build_file_read_row(fr, mtm, file_button_factory));
            }
            Effect::FileWrite(fw) => out.others.push(build_file_write_row(fw, mtm)),
            Effect::ProcessSpawn(ps) => out.others.push(build_process_row(ps, mtm)),
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

/// Single header row: just the header name in bold blue (matching
/// the CLI `HeaderName` style). Values are deliberately not shown
/// — any header value can carry secrets we cannot reliably
/// recognise (custom auth headers, tenant IDs, signed URLs in
/// `Referer`, JWTs in non-standard places, etc.), so the popover
/// treats every value as sensitive and only surfaces the *names*
/// of the headers being sent. The full payload is still reachable
/// via "Show raw", where the §8.5 renderer applies the same
/// unconditional `••••<last-4>` redaction recipe (see
/// `plans/Overview.md` §8.5.1).
fn build_header_row(h: &Header, mtm: MainThreadMarker) -> Retained<NSView> {
    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(4.0);

    let safe_name = sanitize_for_display(&h.name);
    let name = NSTextField::labelWithString(&NSString::from_str(&safe_name), mtm);
    name.setFont(Some(&NSFont::boldSystemFontOfSize(ROW_FONT_SIZE)));
    name.setTextColor(Some(&NSColor::systemBlueColor()));
    row.addArrangedSubview(&name);

    let spacer = NSView::new(mtm);
    row.addArrangedSubview(&spacer);
    row.into_super()
}

/// Build the body section. Returns `None` for `Body::None` so we
/// don't render a stub "body" label for bodyless GETs.
///
/// `file_button_factory` is threaded in so the `Body::FromFile`
/// branch can attach the per-row "Open file" button (Phase 5.1).
/// Other body variants don't reference an on-disk path and ignore
/// the factory.
fn build_body_section(
    body: &Body,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> Option<Retained<NSView>> {
    // `body_meta_label` is the single source of truth for "does this
    // body get a section at all?" — it returns `None` exactly for
    // `Body::None`, so the match below never sees that variant.
    let meta = card_effects::body_meta_label(body)?;
    let content: Option<Retained<NSView>> = match body {
        Body::None => None,
        Body::Inline { bytes } => Some(inline_body_content(bytes, mtm)),
        Body::FromFile { path } => Some(file_glyph_row_with_open(
            "doc.text",
            path,
            mtm,
            file_button_factory,
        )),
        Body::Form { fields } => {
            let stack = NSStackView::new(mtm);
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setSpacing(2.0);
            stack.setDistribution(NSStackViewDistribution::Fill);
            for f in fields {
                let pair = NSTextField::labelWithString(
                    &NSString::from_str(&card_effects::form_field_text(f)),
                    mtm,
                );
                pair.setFont(Some(&monospaced(ROW_FONT_SIZE)));
                stack.addArrangedSubview(&pair);
            }
            Some(stack.into_super())
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
    let text = card_effects::inline_body_text(bytes);
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
    let (label, redacted) = card_effects::auth_label(auth);
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

fn build_file_read_row(
    fr: &FileRead,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> Retained<NSView> {
    let stack = vertical_section("read", mtm);
    stack.addArrangedSubview(&file_glyph_row_with_open(
        "doc.text",
        &fr.path,
        mtm,
        file_button_factory,
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
    let row = file_glyph_row_base(symbol, text, mtm);
    let spacer = NSView::new(mtm);
    row.addArrangedSubview(&spacer);
    row.into_super()
}

/// File-input variant of [`file_glyph_row`] that also asks
/// `file_button_factory` for an "Open file" button (Phase 5.1) and
/// inserts it between the path label and the trailing flexible
/// spacer. The factory returns `None` for paths that don't exist on
/// disk, keeping the row identical to the no-button case for
/// pre-write or vanished paths.
fn file_glyph_row_with_open(
    symbol: &str,
    path: &Path,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> Retained<NSView> {
    let row = file_glyph_row_base(symbol, &path.display().to_string(), mtm);
    if let Some(btn) = file_button_factory(path, mtm) {
        row.addArrangedSubview(&btn);
    }
    let spacer = NSView::new(mtm);
    row.addArrangedSubview(&spacer);
    row.into_super()
}

/// Glyph + label without the trailing spacer, so the spacer can sit
/// after any optional trailing controls (e.g. the Phase 5.1 "Open
/// file" button) and the layout still reads as `[glyph] [label]
/// [controls] <spacer>`.
fn file_glyph_row_base(symbol: &str, text: &str, mtm: MainThreadMarker) -> Retained<NSStackView> {
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

    let label = NSTextField::labelWithString(&NSString::from_str(&sanitize_for_display(text)), mtm);
    label.setFont(Some(&monospaced(ROW_FONT_SIZE)));
    row.addArrangedSubview(&label);

    row
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
