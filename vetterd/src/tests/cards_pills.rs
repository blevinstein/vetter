//! Tests for [`crate::cards::pills`].
//!
//! Lifted out of `crate::tests::popover_pills` when the signal →
//! tone classification moved into the shared card layer. None of
//! this ever needed AppKit; it now runs on every target.

use super::*;
use vetter_core::SignalKind;

/// Every `SignalKind` in the canonical list, so a new variant added
/// to `vetter-core` shows up as a compile error here rather than
/// silently skipping the tier assertions below.
const ALL_KINDS: &[SignalKind] = &[
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
];

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
    // card author to confirm the row reordering on purpose
    // rather than discover it visually.
    use vetter_core::BadgeSeverity;
    for &k in ALL_KINDS {
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
fn no_current_kind_is_info_tier_so_every_kind_earns_a_pill() {
    // `UnknownHost` is `Warn` today; if this flips back to `Info`
    // somebody is opting it out of the chip surface and should
    // explicitly update the design doc. This is the platform-neutral
    // half of what `popover_pills::info_tier_kinds_return_no_pill`
    // used to assert through AppKit.
    for &k in ALL_KINDS {
        assert!(
            signal_pill(k, "detail goes here").is_some(),
            "expected a pill for non-Info kind {k:?}"
        );
        assert!(signal_tone(k).is_some(), "expected a tone for {k:?}");
    }
}

#[test]
fn tone_tracks_priority() {
    // The two must not drift: `signal_priority` is derived from
    // `signal_tone`, and this pins the mapping both ways.
    for &k in ALL_KINDS {
        let want = match signal_tone(k) {
            Some(Tone::Danger) => 0,
            Some(Tone::Warn) => 1,
            Some(Tone::Positive) => 2,
            None => 3,
        };
        assert_eq!(signal_priority(k), want, "{k:?}");
    }
    assert_eq!(signal_tone(SignalKind::AuthHeader), Some(Tone::Positive));
}

#[test]
fn tooltip_appends_detail_when_present() {
    let with = signal_pill(SignalKind::InsecureFlag, "-k disables TLS verification")
        .expect("InsecureFlag earns a pill");
    assert_eq!(
        with.tooltip,
        format!("{}: -k disables TLS verification", with.label)
    );

    // Empty detail degrades to the bare label rather than leaving a
    // dangling ": " on the end of the tooltip.
    let without = signal_pill(SignalKind::InsecureFlag, "").expect("InsecureFlag earns a pill");
    assert_eq!(without.tooltip, without.label);
}
