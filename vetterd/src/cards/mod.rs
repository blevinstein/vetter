//! Platform-neutral card lowering: queue state → display data.
//!
//! Every approval surface vetter ships paints the same §8.5 card
//! catalogue — the macOS popover today, the GTK window in Phase 6d,
//! and whatever comes after. The *decisions* behind those cards are
//! identical on every platform: which trust class a host falls into,
//! which order signal pills sort in, how a `Body::Form` summarises,
//! where an ANSI-styled run starts and stops. Only the widget
//! assembly differs.
//!
//! This module is where those decisions live. Nothing here touches
//! AppKit, GTK, or any toolkit type — it is plain Rust over
//! `vetter_core` types, compiled on every target and unit-tested on
//! every target. `plans/LinuxApp.md` §6d calls for exactly this
//! split: "keep the pure lowering functions shared and unit-tested,
//! and let the GTK module own only widget assembly."
//!
//! The corollary matters as much as the rule: **a function belongs
//! here only if it can be expressed without naming a toolkit type.**
//! Colour *choices* are lowered to semantic tones ([`pills::Tone`],
//! [`url::MethodTone`], [`url::HostTrust`]) and each platform maps a
//! tone onto its own palette — `NSColor::systemRedColor()` on macOS,
//! a CSS class or Pango attribute on GTK. Sizes, fonts, glyph names
//! and layout constants stay with the platform that understands
//! them.
//!
//! ## Layout
//!
//! - [`spans`] — ANSI SGR → styled spans, for the §8.5 detail body.
//! - [`pills`] — risk signals → pill tone, label, tooltip, sort order.
//! - [`url`] — URL row segmentation and host-trust classification.
//! - [`effects`] — `parsed.effects` → per-row summary strings.
//! - [`rules`] — allowlist duration ladder and rule YAML preview.

pub mod effects;
pub mod pills;
pub mod rules;
pub mod spans;
pub mod url;
