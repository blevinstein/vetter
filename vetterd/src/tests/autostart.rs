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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod stub_impl {
    //! On targets with no autostart backend at all the entire
    //! registration path is unreachable; the CLI / popover / doctor
    //! row should observe a consistent "unsupported" state without
    //! blowing up the daemon. Linux has a real backend as of Phase
    //! 6e and is covered by `linux_entry` below instead.
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

#[cfg(target_os = "linux")]
mod linux_entry {
    //! Phase 6e. Autostart on Linux is a file, so unlike the macOS
    //! backend it is fully testable without a desktop session: every
    //! state below is a directory we build in a tempdir.
    use std::path::{Path, PathBuf};

    use super::super::sys;
    use super::*;

    /// A real, existing binary to point `Exec=` at. Using the test
    /// binary itself means "the target exists" is true for reasons
    /// that survive the tempdir being cleaned up.
    fn real_exe() -> PathBuf {
        std::env::current_exe().expect("test binary path")
    }

    fn entry_in(dir: &Path) -> PathBuf {
        dir.join("autostart").join("vetter.desktop")
    }

    #[test]
    fn absent_entry_reads_as_not_registered() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            sys::current_at(&entry_in(dir.path())),
            AutostartStatus::NotRegistered
        );
    }

    #[test]
    fn written_entry_reads_as_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        sys::write_entry_at(&path, &real_exe()).expect("write entry");
        assert_eq!(sys::current_at(&path), AutostartStatus::Enabled);
    }

    #[test]
    fn write_then_remove_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        sys::write_entry_at(&path, &real_exe()).unwrap();
        assert!(path.exists());
        sys::remove_entry_at(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(sys::current_at(&path), AutostartStatus::NotRegistered);
    }

    #[test]
    fn removing_an_absent_entry_is_success() {
        // `reconcile_with_settings` calls `disable()` without first
        // checking, and Apple's `unregister` is a no-op when never
        // registered. Keep the two backends symmetric.
        let dir = tempfile::tempdir().unwrap();
        sys::remove_entry_at(&entry_in(dir.path())).expect("absent removal is not an error");
    }

    #[test]
    fn entry_is_world_readable_but_directory_is_not() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        sys::write_entry_at(&path, &real_exe()).unwrap();
        let entry_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(entry_mode, 0o644, "desktop entries are conventionally 0644");
        assert_eq!(dir_mode, 0o700, "but the directory stays user-private");
    }

    #[test]
    fn hidden_true_reads_as_not_registered() {
        // The XDG spec's way of saying "the user deleted this".
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        sys::write_entry_at(&path, &real_exe()).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap() + "Hidden=true\n";
        std::fs::write(&path, contents).unwrap();
        assert_eq!(sys::current_at(&path), AutostartStatus::NotRegistered);
    }

    #[test]
    fn gnome_disabled_flag_reads_as_not_registered() {
        // What GNOME Tweaks writes when you untick an application.
        // Missing this would let `reconcile_with_settings` silently
        // re-enable autostart the user had just switched off.
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        sys::write_entry_at(&path, &real_exe()).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap().replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=false",
        );
        std::fs::write(&path, contents).unwrap();
        assert_eq!(sys::current_at(&path), AutostartStatus::NotRegistered);
    }

    #[test]
    fn stale_exec_target_reads_as_not_found() {
        // The failure mode this backend has and SMAppService does not:
        // the entry still exists, but the binary it names has moved.
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        let ghost = dir.path().join("moved-away-vetterd");
        std::fs::write(&ghost, b"#!/bin/true\n").unwrap();
        sys::write_entry_at(&path, &ghost).unwrap();
        assert_eq!(sys::current_at(&path), AutostartStatus::Enabled);
        std::fs::remove_file(&ghost).unwrap();
        assert_eq!(sys::current_at(&path), AutostartStatus::NotFound);
    }

    #[test]
    fn entry_without_exec_reads_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[Desktop Entry]\nType=Application\nName=Vetter\n").unwrap();
        assert_eq!(sys::current_at(&path), AutostartStatus::NotFound);
    }

    #[test]
    fn relative_exec_is_not_called_missing() {
        // A bare command name is resolved against $PATH by the session
        // at login. We cannot reproduce that here, so we must not
        // claim it is broken.
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[Desktop Entry]\nExec=vetterd\n").unwrap();
        assert_eq!(sys::current_at(&path), AutostartStatus::Enabled);
    }

    #[test]
    fn exec_path_with_spaces_round_trips() {
        // Quoting has to survive the write/read pair, or an install
        // under "~/My Apps/" would produce an entry that launches the
        // wrong thing — or nothing.
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("dir with spaces");
        std::fs::create_dir_all(&spaced).unwrap();
        let exe = spaced.join("vetterd");
        std::fs::write(&exe, b"#!/bin/true\n").unwrap();
        let path = entry_in(dir.path());
        sys::write_entry_at(&path, &exe).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(sys::exec_target(&contents).as_deref(), Some(exe.as_path()));
        assert_eq!(sys::current_at(&path), AutostartStatus::Enabled);
    }

    #[test]
    fn commented_out_hidden_is_not_a_setting() {
        // The entry we write ends with comment lines; a naive parser
        // that ignored `#` would be one stray comment away from
        // reporting a live entry as disabled.
        assert!(!sys::is_disabled("[Desktop Entry]\n#Hidden=true\n"));
        assert!(sys::is_disabled("[Desktop Entry]\nHidden=true\n"));
    }

    #[test]
    fn rendered_entry_is_a_desktop_entry_naming_the_binary() {
        let rendered = sys::render_entry(Path::new("/usr/bin/vetterd"));
        assert!(rendered.starts_with("[Desktop Entry]\n"));
        assert!(rendered.contains("Type=Application"));
        assert!(rendered.contains("Exec=\"/usr/bin/vetterd\""));
        assert_eq!(
            sys::exec_target(&rendered).as_deref(),
            Some(Path::new("/usr/bin/vetterd"))
        );
    }

    #[test]
    fn requires_approval_is_never_returned() {
        // There is nothing to approve: writing a file in your own
        // config directory needs no permission. The macOS rollback UI
        // for that state must stay unreachable here.
        let dir = tempfile::tempdir().unwrap();
        let path = entry_in(dir.path());
        for setup in [None, Some("Hidden=true\n"), Some("Exec=/nonexistent/x\n")] {
            match setup {
                None => {
                    sys::write_entry_at(&path, &real_exe()).unwrap();
                }
                Some(extra) => {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, format!("[Desktop Entry]\n{extra}")).unwrap();
                }
            }
            assert_ne!(sys::current_at(&path), AutostartStatus::RequiresApproval);
        }
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
