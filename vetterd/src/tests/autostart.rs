//! Tests for [`crate::autostart`]. Layout convention from `AGENTS.md`.
//!
//! The `enable()` / `disable()` round-trip can only really be
//! exercised by a maintainer running the signed `Vetter.app` —
//! `SMAppService` refuses to honour calls from a non-bundled
//! executable, and a CI runner can never satisfy that. We pin the
//! pieces we *can* test in isolation:
//!
//! - The status enum's helpers (`is_enabled`, `label`) so the
//!   doctor row + popover footer don't silently drift.
//! - The bundle-location guard, parameterised on the executable
//!   path so we don't have to spawn anything.
//! - The non-macOS stub, on builds where it's compiled in.

use super::*;

#[test]
fn is_enabled_only_for_enabled_and_requires_approval() {
    assert!(AutostartStatus::Enabled.is_enabled());
    assert!(AutostartStatus::RequiresApproval.is_enabled());
    assert!(!AutostartStatus::NotRegistered.is_enabled());
    assert!(!AutostartStatus::NotFound.is_enabled());
    assert!(!AutostartStatus::Unsupported.is_enabled());
}

#[test]
fn label_strings_are_user_facing() {
    // Pinned so the `vet doctor` row + popover footer don't drift.
    assert_eq!(AutostartStatus::Enabled.label(), "enabled");
    assert_eq!(AutostartStatus::NotRegistered.label(), "disabled");
    assert_eq!(
        AutostartStatus::RequiresApproval.label(),
        "requires approval"
    );
    assert_eq!(AutostartStatus::NotFound.label(), "not found");
    assert_eq!(AutostartStatus::Unsupported.label(), "unsupported");
}

#[cfg(target_os = "macos")]
mod bundle_guard {
    //! Pin both branches of the `.app/Contents/MacOS/<exe>` check
    //! that gates `enable()` / `disable()` on macOS. The helper is
    //! `pub(crate)` on `crate::notifier::mac` and is the same one
    //! the SMAppService driver consults — share the test surface so
    //! a future change to bundle-detection automatically covers
    //! both call sites.
    use std::path::PathBuf;

    use crate::notifier::mac::is_app_bundle_executable;

    #[test]
    fn accepts_canonical_app_layout() {
        let exe = PathBuf::from("/Applications/Vetter.app/Contents/MacOS/vetterd");
        assert!(is_app_bundle_executable(&exe));
    }

    #[test]
    fn accepts_dev_target_layout() {
        // tools/build-app.sh writes its bundle into target/Vetter.app.
        let exe = PathBuf::from("/Users/me/dev/vetter/target/Vetter.app/Contents/MacOS/vetterd");
        assert!(is_app_bundle_executable(&exe));
    }

    #[test]
    fn rejects_bare_target_release() {
        let exe = PathBuf::from("/Users/me/dev/vetter/target/release/vetterd");
        assert!(!is_app_bundle_executable(&exe));
    }

    #[test]
    fn rejects_homebrew_install_path() {
        let exe = PathBuf::from("/opt/homebrew/bin/vetterd");
        assert!(!is_app_bundle_executable(&exe));
    }

    #[test]
    fn rejects_directory_named_macos_outside_app_bundle() {
        // The walk has to verify *every* segment, not just the
        // leaf. A directory called `MacOS` under a non-`.app`
        // ancestor must not slip through.
        let exe = PathBuf::from("/Users/me/random/MacOS/vetterd");
        assert!(!is_app_bundle_executable(&exe));
    }

    #[test]
    fn rejects_app_bundle_with_wrong_contents_dir() {
        let exe = PathBuf::from("/Applications/Vetter.app/NotContents/MacOS/vetterd");
        assert!(!is_app_bundle_executable(&exe));
    }
}

#[cfg(not(target_os = "macos"))]
mod stub_impl {
    //! On non-macOS targets the entire SMAppService path is
    //! unreachable; the CLI / popover / doctor row should observe a
    //! consistent "unsupported" state without blowing up the daemon.
    use super::*;

    #[test]
    fn current_returns_unsupported() {
        assert_eq!(current(), AutostartStatus::Unsupported);
    }

    #[test]
    fn enable_errors_with_unavailable() {
        let err = enable().expect_err("enable must error on non-macOS");
        assert!(matches!(err, AutostartError::Unavailable(_)));
    }

    #[test]
    fn disable_errors_with_unavailable() {
        let err = disable().expect_err("disable must error on non-macOS");
        assert!(matches!(err, AutostartError::Unavailable(_)));
    }

    #[test]
    fn reconcile_is_no_op_when_unsupported() {
        // Even with desired=true, no error and no change should
        // surface (we don't want a Linux daemon to refuse to start
        // because `~/.vet/settings.yaml` says `autostart: true`).
        assert!(!reconcile_with_settings(true).unwrap());
        assert!(!reconcile_with_settings(false).unwrap());
    }
}

/// macOS smoke test guarded by `#[ignore]`. Maintainers running the
/// signed `Vetter.app` can re-enable with
/// `cargo test -p vetterd --test autostart_smoke -- --ignored`. The
/// CI runner never has a signed bundle so it would always fail
/// `enforce_bundle_guard`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires running inside Vetter.app and would mutate Login Items"]
fn enable_then_disable_roundtrip() {
    let original = current();
    enable().expect("enable should succeed inside Vetter.app");
    assert!(
        current().is_enabled(),
        "current() should reflect Enabled after enable()"
    );
    disable().expect("disable should succeed inside Vetter.app");
    assert!(
        !current().is_enabled(),
        "current() should be off after disable()"
    );
    // Restore prior state (best-effort) so the test is idempotent.
    if original.is_enabled() {
        let _ = enable();
    }
}
