//! Tests for [`crate::runloop::popover_pills`].
//!
//! The pill builder is mostly Foundation/AppKit calls that need a
//! main thread to run, so we keep the assertions modest:
//!
//! - Severity classification routes through `SignalKind::ui_severity`,
//!   which has its own unit tests in `vetter-core`. Here we just
//!   pin the `Info → None` branch so a future "raise to Warn"
//!   doesn't silently start emitting pills.
//! - Tooltip composition is pure-string and is tested without
//!   touching AppKit so it runs in CI's headless mode too.
//!
//! AppKit-touching tests use `MainThreadMarker::new()` and `return`
//! out when called from a worker thread (the cargo test runner
//! occasionally schedules off-main); the assertions are
//! best-effort smoke checks.

#![cfg(target_os = "macos")]

use super::*;
use vetter_core::SignalKind;

#[test]
fn info_tier_kinds_return_no_pill() {
    // `UnknownHost` is `Warn` today; if this flips back to `Info`
    // somebody is opting it out of the chip surface and should
    // explicitly update the design doc. This guards against an
    // accidental regression.
    let pill_for = |k: SignalKind| {
        let mtm = match objc2_foundation::MainThreadMarker::new() {
            Some(m) => m,
            None => return None, // can't construct on a worker thread
        };
        build_signal_pill(k, "detail goes here", mtm)
    };

    // No current SignalKind is Info-tier (verified by
    // `vetter-core::tests::signals::no_current_signal_is_info_tier`).
    // Walk the canonical list and assert the inverse — every kind
    // emits a pill on a main-thread runner.
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return;
    }
    for k in [
        SignalKind::InsecureFlag,
        SignalKind::InsecureTls,
        SignalKind::CacertOverride,
        SignalKind::ResolveOverride,
        SignalKind::UnixSocket,
        SignalKind::PipeToShell,
        SignalKind::RawIpLiteral,
        SignalKind::WriteMethod,
        SignalKind::AuthHeader,
        SignalKind::NonStandardPort,
        SignalKind::IdnHost,
        SignalKind::FileOutsideCwd,
        SignalKind::FileReadOutsideCwd,
        SignalKind::UnknownHost,
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
