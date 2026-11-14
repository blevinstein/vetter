//! Tests for [`crate::update_watch`]. Layout convention from `AGENTS.md`.

use std::ffi::OsStr;

use super::*;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn cache(latest: Option<&str>, notified: Option<&str>) -> UpdateCache {
    UpdateCache {
        checked_at: 0,
        latest: latest.map(Into::into),
        daemon_checked_at: None,
        notified_version: notified.map(Into::into),
    }
}

#[test]
fn announces_a_newer_release() {
    let a = announcement_for(&cache(Some("0.3.0"), None), &v("0.2.5")).unwrap();
    assert_eq!(a.latest, v("0.3.0"));
    assert!(a.notice.contains("v0.3.0"), "{}", a.notice);
}

#[test]
fn stays_quiet_when_current_unknown_or_already_announced() {
    assert_eq!(
        announcement_for(&cache(Some("0.2.5"), None), &v("0.2.5")),
        None
    );
    assert_eq!(announcement_for(&cache(None, None), &v("0.2.5")), None);
    assert_eq!(
        announcement_for(&cache(Some("0.3.0"), Some("0.3.0")), &v("0.2.5")),
        None
    );
}

#[test]
fn a_later_release_is_announced_again() {
    assert!(announcement_for(&cache(Some("0.4.0"), Some("0.3.0")), &v("0.2.5")).is_some());
}

#[test]
fn bundle_is_found_from_the_daemon_path() {
    let exe = Path::new("/Applications/Vetter.app/Contents/MacOS/vetterd");
    assert_eq!(
        enclosing_app_bundle(exe),
        Some(PathBuf::from("/Applications/Vetter.app"))
    );
    assert_eq!(
        enclosing_app_bundle(Path::new("/usr/local/bin/vetterd")),
        None
    );
}

#[test]
fn restart_goes_through_open_inside_a_bundle_and_vet_elsewhere() {
    let vet = Path::new("/opt/vet");
    let bundled = restart_command(vet, Some(Path::new("/Applications/Vetter.app")));
    assert_eq!(bundled.get_program(), "/bin/sh");
    let args: Vec<&OsStr> = bundled.get_args().collect();
    assert!(args[1].to_string_lossy().contains("daemon stop"));
    assert_eq!(args[3], "/opt/vet");
    assert_eq!(args[4], "/Applications/Vetter.app");

    let plain = restart_command(vet, None);
    assert_eq!(plain.get_program(), "/opt/vet");
    assert_eq!(plain.get_args().collect::<Vec<_>>(), ["daemon", "restart"]);
}
