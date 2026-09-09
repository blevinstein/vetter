//! Tests for [`crate::doctor`]. Layout convention from `AGENTS.md`.
//!
//! The probes themselves need a session bus, a package manager, or a
//! running daemon, none of which CI has. What is testable — and what
//! actually decides what a user reads — is the *mapping* from a probe
//! outcome to a row: which status it earns, and whether the detail
//! says what still works. Every probe below is therefore split into a
//! `probe_*` half that touches the world and a pure half that takes
//! the outcome as an argument; these cover the pure half.

use super::*;

/// A row must never claim a capability the probe did not observe.
/// This is the invariant the whole group exists to protect: `Ok` is
/// reserved for "we looked and it was there".
#[cfg(target_os = "linux")]
mod desktop_rows {
    use super::*;
    use vetter_core::wire::DesktopHealth;

    fn health(server: Option<&str>, actions: bool, watcher: bool, host: bool) -> DesktopHealth {
        DesktopHealth {
            session_bus: true,
            notification_server: server.map(str::to_string),
            notification_actions: actions,
            tray_watcher: watcher,
            tray_host: host,
        }
    }

    #[test]
    fn notifications_ok_only_when_a_server_advertises_actions() {
        let h = health(Some("Plasma"), true, true, true);
        let row = notification_row(Some(&h));
        assert_eq!(row.status, Status::Ok);
        assert!(row.detail.contains("Plasma"), "detail: {}", row.detail);
    }

    #[test]
    fn notifications_without_actions_warns_and_names_the_fallback() {
        // Degradation, not failure: banners still appear, and §5.4's
        // whole point is that the body click still reaches the window.
        // A row that just said "WARN no actions" would leave the user
        // thinking approvals were broken.
        let h = health(Some("notify-osd"), false, true, true);
        let row = notification_row(Some(&h));
        assert_eq!(row.status, Status::Warn);
        assert!(row.detail.contains("notify-osd"));
        assert!(
            row.detail.contains("body"),
            "should point at the body click: {}",
            row.detail
        );
    }

    #[test]
    fn no_notification_server_names_the_remaining_paths() {
        let h = health(None, false, true, true);
        let row = notification_row(Some(&h));
        assert_eq!(row.status, Status::Warn);
        assert!(
            row.detail.contains("vet daemon approve"),
            "should name a path that still works: {}",
            row.detail
        );
    }

    #[test]
    fn tray_absent_is_info_not_a_failure() {
        // GNOME's out-of-the-box state. It costs the tray icon and
        // nothing else, so flagging it as a problem would train users
        // to ignore the report.
        let h = health(Some("gnome-shell"), true, false, false);
        let row = tray_row(Some(&h));
        assert_eq!(row.status, Status::Info);
    }

    #[test]
    fn watcher_without_a_host_warns() {
        // The genuinely confusing case: our item registers fine and
        // then nobody draws it, which looks like a Vetter bug.
        let h = health(Some("gnome-shell"), true, true, false);
        let row = tray_row(Some(&h));
        assert_eq!(row.status, Status::Warn);
        assert!(
            row.detail.contains("no host"),
            "must distinguish this from having no watcher: {}",
            row.detail
        );
    }

    #[test]
    fn rows_report_not_probed_when_the_daemon_is_down() {
        // Never OK, never WARN: we did not look, and saying either
        // would be a claim we cannot support.
        for row in [notification_row(None), tray_row(None)] {
            assert_eq!(row.status, Status::Info);
            assert!(row.detail.contains("not probed"), "detail: {}", row.detail);
        }
    }
}

#[cfg(target_os = "linux")]
mod bus_and_runtime_rows {
    use super::*;

    #[test]
    fn unreachable_bus_warns_and_names_the_headless_path() {
        let row = session_bus_row(BusProbe::Unreachable {
            address: "unix:path=/run/user/1000/bus".into(),
            why: "connection refused".into(),
        });
        assert_eq!(row.status, Status::Warn);
        assert!(row.detail.contains("vet daemon approve"));
    }

    #[test]
    fn unset_bus_is_info_because_headless_is_supported() {
        let row = session_bus_row(BusProbe::Unset);
        assert_eq!(row.status, Status::Info);
    }

    #[test]
    fn unverifiable_address_form_is_not_reported_as_reachable() {
        let row = session_bus_row(BusProbe::NotProbeable("tcp:host=localhost".into()));
        assert_eq!(row.status, Status::Info);
        assert!(row.detail.contains("cannot verify"));
    }

    #[test]
    fn runtime_dir_owned_by_someone_else_is_an_error() {
        // The socket, admin socket and pidfile all live here, so a
        // foreign owner is a genuine problem rather than a missing
        // nicety — the one row in this group that earns ERROR.
        let row = runtime_dir_row(RuntimeDirProbe::Foreign {
            path: "/run/user/1000".into(),
            owner: 0,
        });
        assert_eq!(row.status, Status::Error);
    }

    #[test]
    fn loose_runtime_dir_warns_with_the_repair() {
        let row = runtime_dir_row(RuntimeDirProbe::Loose {
            path: "/run/user/1000".into(),
            mode: 0o755,
        });
        assert_eq!(row.status, Status::Warn);
        assert!(row.detail.contains("0755"), "detail: {}", row.detail);
        assert!(row.detail.contains("chmod"), "detail: {}", row.detail);
    }
}

#[cfg(target_os = "linux")]
mod provenance_rows {
    use super::*;

    #[test]
    fn a_package_is_the_only_ok_state() {
        assert_eq!(
            provenance_row(Provenance::Package("vetter".into())).status,
            Status::Ok
        );
        // Everything else must not read as verified provenance.
        for p in [
            Provenance::Unpackaged,
            Provenance::NoPackageManager,
            Provenance::Unresolved("nope".into()),
        ] {
            assert_ne!(provenance_row(p).status, Status::Ok);
        }
    }

    #[test]
    fn source_builds_are_info_and_say_it_is_expected() {
        // Today this is essentially every install: packaging is
        // deferred to Phase 6f. It must not read as a warning, or the
        // report cries wolf for every developer.
        let row = provenance_row(Provenance::Unpackaged);
        assert_eq!(row.status, Status::Info);
        assert!(row.detail.contains("built from source"));
        assert!(row.detail.contains("expected"));
    }

    #[test]
    fn an_unlocatable_binary_warns() {
        let row = provenance_row(Provenance::Unresolved("no vetterd on PATH".into()));
        assert_eq!(row.status, Status::Warn);
        assert!(row.detail.contains("no vetterd on PATH"));
    }
}

#[cfg(target_os = "linux")]
mod desktop_entry_row_tests {
    use super::*;

    #[test]
    fn missing_entry_is_info_and_names_the_installer() {
        // Absent, this is invisible: notifications and the window just
        // look generic. The row exists to make it visible, not to
        // treat it as breakage.
        let row = desktop_entry_row(None);
        assert_eq!(row.status, Status::Info);
        assert!(row.detail.contains("install-desktop.sh"));
    }

    #[test]
    fn present_entry_reports_the_path_it_found() {
        let row = desktop_entry_row(Some(PathBuf::from(
            "/home/u/.local/share/applications/dev.vetter.daemon.desktop",
        )));
        assert_eq!(row.status, Status::Ok);
        assert!(row.detail.contains(".local/share"));
    }
}
