//! Tests for the pieces of [`super`] that are decisions rather than
//! widget assembly.
//!
//! Most of this module is GTK calls, which need a display and are
//! covered by the manual smoke in `plans/LinuxApp.md` §7. What is
//! testable is the palette mapping the widget code *reads*.
//!
//! The layout invariants (size floor vs. default, card spacing vs.
//! row spacing) are `const` assertions next to the constants
//! themselves instead: the compiler knows those answers, so a
//! violation should fail the build rather than wait for a test run.
//! Styling is deliberately not asserted — a test restating the CSS
//! string would fail on every visual change while catching nothing.

use super::*;

/// The two palettes must actually differ. A copy-paste that left dark
/// pointing at the light values would still compile, still pass any
/// "is it a palette" check, and produce an unreadable §8.5 body on
/// exactly the theme most developers run.
#[test]
fn light_and_dark_palettes_are_distinct() {
    let light = palette_for(false);
    let dark = palette_for(true);
    assert_ne!(light.red, dark.red);
    assert_ne!(light.green, dark.green);
    assert_ne!(light.dim, dark.dim);
}

#[test]
fn palette_for_true_is_the_dark_palette() {
    assert_eq!(palette_for(true).red, DARK_PALETTE.red);
    assert_eq!(palette_for(false).red, LIGHT_PALETTE.red);
}

/// Every palette slot has to be a colour GTK will parse, because a
/// malformed one does not fail loudly — Pango drops the attribute and
/// the span silently renders unstyled, which on the `Danger` red is
/// the difference between a warning a user sees and one they do not.
#[test]
fn every_palette_slot_is_a_hex_colour() {
    for palette in [palette_for(false), palette_for(true)] {
        for value in [
            palette.red,
            palette.green,
            palette.yellow,
            palette.magenta,
            palette.cyan,
            palette.blue,
            palette.dim,
        ] {
            assert!(
                value.len() == 7
                    && value.starts_with('#')
                    && value[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "`{value}` is not a #rrggbb colour"
            );
        }
    }
}
