//! Tests for [`crate::runloop::popover_pills`].
//!
//! Only the AppKit half lives here now. Which signals earn a pill,
//! what the chip says, and how the row sorts moved into the shared
//! card layer along with their assertions — see
//! `crate::tests::cards_pills`, which runs on every target.
//!
//! What remains is Foundation/AppKit construction, which needs a
//! main thread to run, so the assertions are modest smoke checks.
//! AppKit-touching tests use `MainThreadMarker::new()` and `return`
//! out when called from a worker thread (the cargo test runner
//! occasionally schedules off-main).

#![cfg(target_os = "macos")]

use super::*;
use vetter_core::SignalKind;

#[test]
fn every_non_info_kind_builds_a_pill_view() {
    // The tone classification is pinned in `cards_pills`; this is
    // the AppKit-side counterpart, confirming the tone actually
    // reaches a constructed `NSView` rather than falling off a
    // match arm.
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return;
    }
    let pill_for = |k: SignalKind| {
        let mtm = objc2_foundation::MainThreadMarker::new()?;
        // Same composition `popover.rs` uses: the shared layer
        // decides whether the kind earns a chip, this side paints it.
        let spec = crate::cards::pills::signal_pill(k, "detail goes here")?;
        Some(build_spec_pill(&spec, mtm))
    };
    for k in [
        SignalKind::InsecureFlag,
        SignalKind::InsecureTls,
        SignalKind::CacertOverride,
        SignalKind::ResolveOverride,
        SignalKind::UnixSocket,
        SignalKind::PipeToShell,
        SignalKind::RawIpLiteral,
        SignalKind::ClientCertificate,
        SignalKind::WriteMethod,
        SignalKind::AuthHeader,
        SignalKind::NonStandardPort,
        SignalKind::IdnHost,
        SignalKind::FileOutsideCwd,
        SignalKind::FileReadOutsideCwd,
        SignalKind::UnknownHost,
        SignalKind::RemoteHeaderName,
        SignalKind::CreateDirs,
    ] {
        assert!(
            pill_for(k).is_some(),
            "expected pill for non-Info kind {k:?}"
        );
    }
}

#[test]
fn build_pill_smoke_runs_without_panicking() {
    // Foundation/AppKit smoke test: build a pill on the main thread
    // (when available) and confirm we get a non-null `NSView`. The
    // colour / font runs are exercised by visual smoke tests in
    // `verify` step 5, not here.
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return; // worker thread — Foundation half isn't reachable
    };
    let fg = objc2_app_kit::NSColor::systemRedColor();
    let bg = objc2_app_kit::NSColor::systemRedColor().colorWithAlphaComponent(0.15);
    let view = build_pill("test-pill", &fg, &bg, "hover detail", mtm);
    // `into_super` returns a non-null `Retained<NSView>`; just pin
    // that we got something back.
    let _ = &*view;
}
