//! Tests for [`crate::update`]. Layout convention from `AGENTS.md`.

use super::*;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

#[test]
fn older_running_version_is_behind() {
    assert_eq!(
        staleness(Some(&v("0.2.4")), &v("0.2.5")),
        Staleness::Behind {
            running: Some(v("0.2.4")),
            latest: v("0.2.5")
        }
    );
}

#[test]
fn equal_or_newer_running_version_is_current() {
    assert_eq!(
        staleness(Some(&v("0.2.5")), &v("0.2.5")),
        Staleness::Current
    );
    assert_eq!(
        staleness(Some(&v("0.3.0")), &v("0.2.5")),
        Staleness::Current
    );
}

#[test]
fn unanswered_daemon_is_behind_with_no_running_version() {
    assert_eq!(
        staleness(None, &v("0.2.5")),
        Staleness::Behind {
            running: None,
            latest: v("0.2.5")
        }
    );
}

#[test]
fn parses_brew_cask_json() {
    let json = r#"{"casks":[{"token":"vetter","version":"0.2.4"}]}"#;
    assert_eq!(parse_brew_cask_version(json).unwrap(), v("0.2.4"));
}

#[test]
fn brew_json_without_a_cask_is_an_error() {
    assert!(matches!(
        parse_brew_cask_version(r#"{"casks":[]}"#),
        Err(UpdateError::MissingVersion)
    ));
}

#[test]
fn parses_github_release_tag_with_leading_v() {
    assert_eq!(
        parse_github_release_version(r#"{"tag_name":"v0.2.5"}"#).unwrap(),
        v("0.2.5")
    );
}

#[test]
fn unparsable_version_is_an_error() {
    assert!(matches!(
        parse_github_release_version(r#"{"tag_name":"nightly"}"#),
        Err(UpdateError::BadVersion(..))
    ));
}

#[test]
fn notice_names_both_versions_or_the_missing_one() {
    let known = notice_text(Some(&v("0.2.4")), &v("0.2.5"));
    assert!(
        known.contains("v0.2.4") && known.contains("v0.2.5"),
        "{known}"
    );
    let unknown = notice_text(None, &v("0.2.5"));
    assert!(unknown.contains("predates version reporting"), "{unknown}");
}

#[test]
fn cache_freshness_and_daemon_check_windows() {
    let cache = UpdateCache {
        checked_at: 1_000,
        latest: Some("0.2.5".into()),
        daemon_checked_at: None,
        notified_version: None,
    };
    assert!(cache.is_fresh(1_000 + CACHE_TTL_SECS - 1));
    assert!(!cache.is_fresh(1_000 + CACHE_TTL_SECS));
    assert!(cache.daemon_check_due(1_000));

    let checked = UpdateCache {
        daemon_checked_at: Some(2_000),
        ..cache
    };
    assert!(!checked.daemon_check_due(2_000 + DAEMON_CHECK_INTERVAL_SECS - 1));
    assert!(checked.daemon_check_due(2_000 + DAEMON_CHECK_INTERVAL_SECS));
}

#[test]
fn cache_round_trips_at_mode_0600_and_bad_files_read_as_absent() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".vet").join("update-check.yaml");
    assert_eq!(load_cache_from(&path), None);

    let cache = UpdateCache {
        checked_at: 42,
        latest: Some("0.2.5".into()),
        daemon_checked_at: Some(7),
        notified_version: Some("0.2.5".into()),
    };
    store_cache_to(&path, &cache).unwrap();
    assert_eq!(load_cache_from(&path), Some(cache));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    std::fs::write(&path, "not: [valid").unwrap();
    assert_eq!(load_cache_from(&path), None);
}

#[test]
fn a_failed_lookup_keeps_what_was_known() {
    let previous = UpdateCache {
        checked_at: 1,
        latest: Some("0.2.5".into()),
        daemon_checked_at: Some(2),
        notified_version: Some("0.2.5".into()),
    };
    let after = UpdateCache::after_lookup(Some(&previous), 99, None);
    assert_eq!(after.checked_at, 99);
    assert_eq!(after.latest, previous.latest);
    assert_eq!(after.daemon_checked_at, Some(2));
    assert_eq!(after.notified_version, previous.notified_version);

    let newer = UpdateCache::after_lookup(Some(&previous), 100, Some("0.3.0".into()));
    assert_eq!(newer.latest.as_deref(), Some("0.3.0"));
}
