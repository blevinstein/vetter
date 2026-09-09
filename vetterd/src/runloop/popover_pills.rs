//! Pill widgets for the approver popover.
//!
//! A "pill" is a small tinted-background label used wherever the UI
//! wants to call out a discrete piece of metadata: a risk-signal
//! kind, a host-trust class, a method badge. The recipe is shared
//! across all three callers so they stay visually consistent (same
//! corner radius, same font weight, same padding).
//!
//! The base [`build_pill`] helper takes raw foreground / background
//! colours and a label string; the higher-level [`build_spec_pill`]
//! paints a [`PillSpec`] — which signals earn a chip, what it says,
//! how urgent it reads and where it sorts are all decided in
//! [`crate::cards::pills`], shared with the GTK surface, per the table
//! in [plans/ApprovalUI.md "Element catalogue"](../../../plans/ApprovalUI.md).
//!
//! `Info`-tier signal kinds intentionally don't get a pill — those
//! lines stay in the §8.5 raw body inside the "Show raw" disclosure.
//! The shared layer drops them, so everything reaching
//! [`build_spec_pill`] is a chip that should be painted.

#![cfg(target_os = "macos")]

use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSFont, NSTextField, NSView};
use objc2_foundation::{MainThreadMarker, NSString};

use crate::cards::pills::{PillSpec, Tone};

/// `CGFloat` is `f64` on Apple Silicon (and `f32` on the legacy
/// 32-bit ABI we don't target). Aliased here so the `setCornerRadius:`
/// `msg_send!` site reads the right type even if a cross-arch build
/// ever lands.
type CGFloat = f64;

/// Tinted-pill body font size. Matches the pill recipe in
/// `plans/ApprovalUI.md` ("Element catalogue").
const PILL_FONT_SIZE: f64 = 10.0;

/// Corner radius for the pill's tinted background. 7pt against a
/// 10pt label produces a roughly capsule-shaped chip.
const PILL_CORNER_RADIUS: f64 = 7.0;

/// Build a tinted pill `NSView` with `label` text in `fg` over `bg`.
///
/// `tooltip` is set verbatim via `setToolTip`; pass an empty string
/// to skip the tooltip.
///
/// Layout details (kept here so callers don't reinvent them):
/// - Two leading + two trailing spaces around `label` give the pill
///   a few points of horizontal padding without needing an
///   `NSView` subclass — `NSTextField`'s intrinsic size grows with
///   the visible text.
/// - `setWantsLayer(true)` is required before reading `layer()` to
///   set the corner radius; AppKit otherwise returns `None`.
pub fn build_pill(
    label: &str,
    fg: &NSColor,
    bg: &NSColor,
    tooltip: &str,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let padded = format!("  {label}  ");
    let field = NSTextField::labelWithString(&NSString::from_str(&padded), mtm);
    field.setBezeled(false);
    field.setBordered(false);
    field.setDrawsBackground(true);
    field.setBackgroundColor(Some(bg));
    field.setTextColor(Some(fg));
    field.setFont(Some(&NSFont::boldSystemFontOfSize(PILL_FONT_SIZE)));
    field.setWantsLayer(true);
    if let Some(layer) = field.layer() {
        // `CALayer::setCornerRadius:` lives in `objc2-quartz-core`
        // behind a `CGFloat` feature gate we don't enable (we don't
        // use anything else from QuartzCore today). The selectors
        // below are stable AppKit since 10.5; sending them directly
        // via `msg_send!` keeps the dependency graph small.
        //
        // `masksToBounds: true` is what actually makes the corner
        // rounding *visible*: without it the layer's geometry is
        // rounded but the underlying `NSTextField` cell still paints
        // a rectangular background that pokes out of the corners.
        // With it, the cell drawing is clipped to the rounded
        // shape — which is what the pill is supposed to look like.
        let radius: CGFloat = PILL_CORNER_RADIUS;
        unsafe {
            let _: () = objc2::msg_send![&*layer, setCornerRadius: radius];
            let _: () = objc2::msg_send![&*layer, setMasksToBounds: true];
        }
    }
    if !tooltip.is_empty() {
        field.setToolTip(Some(&NSString::from_str(tooltip)));
    }
    field.into_super().into_super()
}

/// Paint one [`PillSpec`] as a tinted chip.
///
/// Everything *decided* about the pill — whether the signal earns one
/// at all, its label, its tooltip, its tone and its position in the
/// row — happens in [`crate::cards::pills`], which the GTK surface
/// consumes too (including why `AuthHeader` is special-cased to a
/// positive tone). All that is left here is mapping a tone onto
/// AppKit's palette.
///
/// Each pill is tinted with its own foreground colour at low alpha
/// ([`pill_bg_for`]). The popover itself is forced into Dark Aqua
/// (see `popover.rs`), so a soft red/orange/green tint over a dark
/// surface gives the pill a readable "highlighted capsule" look in
/// both Light and Dark system themes — the popover's effective
/// appearance is fixed regardless. Earlier rounds tried a single
/// near-black background for every pill, but against the now-dark
/// popover that disappeared into the surface.
pub fn build_spec_pill(spec: &PillSpec, mtm: MainThreadMarker) -> Retained<NSView> {
    let fg = match spec.tone {
        Tone::Danger => NSColor::systemRedColor(),
        Tone::Warn => NSColor::systemOrangeColor(),
        Tone::Positive => NSColor::systemGreenColor(),
    };
    let bg = pill_bg_for(&fg);
    build_pill(spec.label, &fg, &bg, &spec.tooltip, mtm)
}

/// Tint helper: returns `fg` faded to a low-alpha background suitable
/// for a pill capsule on the popover's dark surface. Centralised so
/// every caller (signal pills, host pill, dry-run pill) uses the
/// same alpha and gets a consistent visual weight.
pub fn pill_bg_for(fg: &NSColor) -> Retained<NSColor> {
    fg.colorWithAlphaComponent(0.22)
}

#[cfg(test)]
#[path = "../tests/popover_pills.rs"]
mod tests;
