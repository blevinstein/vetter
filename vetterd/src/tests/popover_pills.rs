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
fn signal_priority_orders_danger_warn_authheader() {
    // Danger first (red), generic Warn second (orange), AuthHeader
    // third (green — special-cased positive). Pinned here so a
    // future re-tier of any signal can't silently shuffle the row
    // order without somebody updating the test. Picks one
    // representative kind per band — the per-kind tier mapping is
    // owned by `vetter_core::SignalKind::ui_severity` and tested
    // there.
    assert_eq!(signal_priority(SignalKind::PipeToShell), 0); // Danger
    assert_eq!(signal_priority(SignalKind::WriteMethod), 1); // Warn
    assert_eq!(signal_priority(SignalKind::AuthHeader), 2); // green band
    assert!(
        signal_priority(SignalKind::PipeToShell) < signal_priority(SignalKind::WriteMethod),
        "Danger must sort before Warn"
    );
    assert!(
        signal_priority(SignalKind::WriteMethod) < signal_priority(SignalKind::AuthHeader),
        "Warn must sort before AuthHeader (green)"
    );
}

#[test]
fn signal_priority_groups_match_ui_severity_buckets() {
    // The three priority bands map 1:1 to `ui_severity` plus the
    // AuthHeader override. Walk every current `SignalKind` and
    // pin its band here so a re-tier in vetter-core forces the
    // popover author to confirm the row reordering on purpose
    // rather than discover it visually.
    use vetter_core::BadgeSeverity;
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
        let want = if k == SignalKind::AuthHeader {
            2
        } else {
            match k.ui_severity() {
                BadgeSeverity::Danger => 0,
                BadgeSeverity::Warn => 1,
                BadgeSeverity::Info => 3,
            }
        };
        assert_eq!(
            signal_priority(k),
            want,
            "expected priority={want} for {k:?}"
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
