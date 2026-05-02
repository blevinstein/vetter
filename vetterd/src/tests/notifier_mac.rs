//! Tests for [`crate::notifier::mac`]. Layout convention is described
//! in `AGENTS.md`. macOS-only because the source module is
//! `cfg(target_os = "macos")`.

#![cfg(target_os = "macos")]

use std::path::PathBuf;

use super::*;
use crate::notifier::NotifierBuildError;

/// Canonical bundle layout: `…/Vetter.app/Contents/MacOS/vetterd`.
/// `verify_bundle_path` accepts this — the same Launch Services
/// "the executable lives inside a `.app`" property the bundle's
/// `LSEnvironment` and `UNUserNotificationCenter` registration
/// depend on.
#[test]
fn verify_accepts_canonical_app_bundle_layout() {
    let exe = PathBuf::from("/Applications/Vetter.app/Contents/MacOS/vetterd");
    verify_bundle_path(&exe).expect("canonical bundle path should be accepted");
}

/// `cargo run --bin vetterd` and `vet daemon start` (when it can't
/// find a bundle) both end up at a path like `target/release/vetterd`.
/// We refuse with a `Setup` error so `vetterd::main` exits 78
/// instead of bringing up a half-working UI that hangs every
/// prompt-class request.
#[test]
fn verify_rejects_bare_target_path() {
    let exe = PathBuf::from("/Users/dev/repo/target/release/vetterd");
    let err = verify_bundle_path(&exe).expect_err("bare-binary path must be rejected");
    let msg = err.to_string();
    assert!(matches!(err, NotifierBuildError::Setup(_)));
    assert!(msg.contains("VETTERD_NOTIFIER=mac"), "{msg}");
    // The message must point the user at both fixes (bundle launch
    // and the test opt-out) so the operator immediately knows what
    // to do.
    assert!(msg.contains("Vetter.app"), "{msg}");
    assert!(msg.contains("VETTERD_NOTIFIER=noop"), "{msg}");
    // Helpful: include the actual path we observed.
    assert!(msg.contains(exe.to_str().unwrap()), "{msg}");
}

/// Edge cases: a `.app` ancestor that's not the immediate
/// `Contents/MacOS/` parent must not be enough. We refuse paths
/// that look like they live in some sibling of the bundle so a
/// nested workspace named `target/Foo.app/scratch/vetterd` doesn't
/// trick the check.
#[test]
fn verify_rejects_dot_app_ancestor_not_in_macos_dir() {
    let exe = PathBuf::from("/Users/dev/Vetter.app/scratch/vetterd");
    assert!(verify_bundle_path(&exe).is_err());
}

#[test]
fn verify_rejects_macos_dir_without_dot_app_grandparent() {
    let exe = PathBuf::from("/usr/local/Contents/MacOS/vetterd");
    assert!(verify_bundle_path(&exe).is_err());
}
