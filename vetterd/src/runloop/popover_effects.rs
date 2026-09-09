//! Per-effect native rows for the approver popover.
//!
//! [`build_effect_views`] lowers `parsed.effects` through
//! [`crate::cards::rows::effect_rows`] — shared with the GTK
//! surface — and paints one `NSView` per resulting row, ready to be
//! appended to the card stack:
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
use vetter_core::ParsedCommand;

use crate::cards::rows::{self as card_rows, BodyContent, EffectRow, FilePath};

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
/// Which effects earn a row, in what order, and which bucket they
/// land in are decided by [`crate::cards::rows::effect_rows`], shared
/// with the GTK surface so the two cannot drift. That includes the
/// de-duplication the curl parser makes necessary: it intentionally
/// emits **both** `Body::FromFile { path }` (for `-d @file`) **and** a
/// separate `Effect::FileRead { path }` so the matcher / audit log can
/// reason about the read independently of the HTTP body, and a card
/// painting both would show the same path twice. Only the visible row
/// collapses — the effect list the matcher sees is untouched.
///
/// What is left here is painting: one `NSView` per lowered row.
pub fn build_effect_views(
    parsed: &ParsedCommand,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> EffectViews {
    let rows = card_rows::effect_rows(parsed);
    EffectViews {
        file_inputs: rows
            .file_inputs
            .iter()
            .map(|row| build_row_view(row, mtm, file_button_factory))
            .collect(),
        others: rows
            .others
            .iter()
            .map(|row| build_row_view(row, mtm, file_button_factory))
            .collect(),
    }
}

/// Paint one lowered row.
///
/// Every arm is infallible: the shared layer emits a row only when
/// there is something to draw, so the "empty headers list" and
/// "`Body::None`" cases that used to be `None` returns here never
/// reach this point.
fn build_row_view(
    row: &EffectRow,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> Retained<NSView> {
    match row {
        EffectRow::Headers(names) => build_headers_section(names, mtm),
        EffectRow::Body { meta, content } => {
            build_body_section(meta, content, mtm, file_button_factory)
        }
        EffectRow::Auth { text, redacted } => build_auth_row(text, *redacted, mtm),
        EffectRow::FileRead(path) => build_file_read_row(path, mtm, file_button_factory),
        EffectRow::FileWrite(path) => build_file_write_row(path, mtm),
        EffectRow::ProcessSpawn(command) => build_process_row(command, mtm),
    }
}

/// Build the headers section. `names` is non-empty and already
/// sanitised by the shared layer.
fn build_headers_section(names: &[String], mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = vertical_section("headers", mtm);
    for name in names {
        stack.addArrangedSubview(&build_header_row(name, mtm));
    }
    stack.into_super()
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
fn build_header_row(name: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(4.0);

    let name = NSTextField::labelWithString(&NSString::from_str(name), mtm);
    name.setFont(Some(&NSFont::boldSystemFontOfSize(ROW_FONT_SIZE)));
    name.setTextColor(Some(&NSColor::systemBlueColor()));
    row.addArrangedSubview(&name);

    let spacer = NSView::new(mtm);
    row.addArrangedSubview(&spacer);
    row.into_super()
}

/// Build the body section. Bodyless GETs never reach here — the
/// shared layer emits no body row for `Body::None`, so there is no
/// stub "body" label to suppress.
///
/// `file_button_factory` is threaded in so the
/// [`BodyContent::FromFile`] branch can attach the per-row "Open
/// file" button (Phase 5.1). Other body shapes don't reference an
/// on-disk path and ignore the factory.
fn build_body_section(
    meta: &str,
    content: &BodyContent,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> Retained<NSView> {
    let content_view: Retained<NSView> = match content {
        BodyContent::Inline(text) => inline_body_content(text, mtm),
        BodyContent::FromFile(path) => {
            file_glyph_row_with_open("doc.text", path, mtm, file_button_factory)
        }
        BodyContent::Form(fields) => {
            let stack = NSStackView::new(mtm);
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setSpacing(2.0);
            stack.setDistribution(NSStackViewDistribution::Fill);
            for field in fields {
                let pair = NSTextField::labelWithString(&NSString::from_str(field), mtm);
                pair.setFont(Some(&monospaced(ROW_FONT_SIZE)));
                stack.addArrangedSubview(&pair);
            }
            stack.into_super()
        }
    };

    let stack = vertical_section("body", mtm);
    let meta_label = NSTextField::labelWithString(&NSString::from_str(meta), mtm);
    meta_label.setFont(Some(&NSFont::systemFontOfSize(ROW_FONT_SIZE - 1.0)));
    meta_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    stack.addArrangedSubview(&meta_label);
    stack.addArrangedSubview(&content_view);
    stack.into_super()
}

/// Render the inline bytes either as a UTF-8 string (when valid)
/// or a short hex dump (when not). Wrapped in a label rather than
/// a scrollable text view because bodies in v1 are usually short
/// (form posts, JSON deltas); long bodies are still reachable via
/// "Show raw" and the future scroll-on-hover treatment is tracked
/// in the design doc.
fn inline_body_content(text: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&monospaced(ROW_FONT_SIZE)));
    label.into_super().into_super()
}

/// Auth row. Unauthenticated requests never reach here — the shared
/// layer emits no auth row for them, so there is no "auth: (none)"
/// line to suppress.
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
fn build_auth_row(text: &str, redacted: bool, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = vertical_section("auth", mtm);
    let value = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    value.setFont(Some(&monospaced(ROW_FONT_SIZE)));
    if redacted {
        value.setTextColor(Some(&NSColor::systemGreenColor()));
    } else {
        value.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    stack.addArrangedSubview(&value);
    stack.into_super()
}

fn build_file_read_row(
    path: &FilePath,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> Retained<NSView> {
    let stack = vertical_section("read", mtm);
    stack.addArrangedSubview(&file_glyph_row_with_open(
        "doc.text",
        path,
        mtm,
        file_button_factory,
    ));
    stack.into_super()
}

fn build_file_write_row(path: &FilePath, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = vertical_section("write", mtm);
    stack.addArrangedSubview(&file_glyph_row("square.and.pencil", &path.display, mtm));
    stack.into_super()
}

fn build_process_row(command: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    let stack = vertical_section("spawn", mtm);
    stack.addArrangedSubview(&file_glyph_row("terminal", command, mtm));
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
    path: &FilePath,
    mtm: MainThreadMarker,
    file_button_factory: &FileButtonFactory<'_>,
) -> Retained<NSView> {
    let row = file_glyph_row_base(symbol, &path.display, mtm);
    if let Some(btn) = file_button_factory(&path.path, mtm) {
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

    // Every caller now passes text the shared layer already
    // sanitised. `sanitize_for_display` is idempotent — its output
    // contains only ordinary printable characters, none of which are
    // in its own deny ranges — so this stays as a belt-and-braces
    // guard on the lowest-level text sink in the module rather than
    // relying on every future caller remembering.
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
