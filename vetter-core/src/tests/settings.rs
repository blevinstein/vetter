//! Tests for [`crate::settings`]. Layout convention from `AGENTS.md`.

use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn default_settings_match_opt_in_and_opt_out_expectations() {
    let s = Settings::default();
    assert!(!s.autostart, "autostart must be opt-in by default");
    assert!(
        s.notification_sound,
        "notification_sound has no security implication, so it defaults on"
    );
}

#[test]
fn missing_file_returns_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    let s = load_from(&path).expect("missing file → default, no error");
    assert_eq!(s, Settings::default());
}

#[test]
fn roundtrip_preserves_autostart_true() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    write_to(
        &path,
        &Settings {
            autostart: true,
            notification_sound: true,
        },
    )
    .unwrap();
    let round = load_from(&path).unwrap();
    assert!(round.autostart);
}

#[test]
fn roundtrip_preserves_notification_sound_false() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    write_to(
        &path,
        &Settings {
            autostart: false,
            notification_sound: false,
        },
    )
    .unwrap();
    let round = load_from(&path).unwrap();
    assert!(!round.notification_sound);
}

#[test]
fn write_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join(".vet").join("settings.yaml");
    write_to(
        &path,
        &Settings {
            autostart: true,
            notification_sound: true,
        },
    )
    .unwrap();
    assert!(path.exists(), "parent dirs should be created");
    let round = load_from(&path).unwrap();
    assert!(round.autostart);
}

#[test]
fn write_lands_mode_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    write_to(&path, &Settings::default()).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "settings file must land mode 0600, got {mode:o}"
    );
}

#[test]
fn write_replaces_atomically_with_no_leftover_tempfiles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    write_to(
        &path,
        &Settings {
            autostart: false,
            notification_sound: true,
        },
    )
    .unwrap();
    write_to(
        &path,
        &Settings {
            autostart: true,
            notification_sound: true,
        },
    )
    .unwrap();
    let round = load_from(&path).unwrap();
    assert!(round.autostart);
    let leaked: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != "settings.yaml")
        .collect();
    assert!(leaked.is_empty(), "leftover tempfile(s): {leaked:?}");
}

#[test]
fn unknown_field_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "autostart: true\nbogus_unknown_field: 42\n").unwrap();
    let err = load_from(&path).expect_err("unknown field must be rejected");
    match err {
        SettingsError::Yaml { .. } => {}
        other => panic!("expected SettingsError::Yaml, got {other:?}"),
    }
}

#[test]
fn missing_autostart_key_defaults_to_off() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "{}\n").unwrap();
    let s = load_from(&path).unwrap();
    assert!(!s.autostart);
}

#[test]
fn missing_notification_sound_key_defaults_to_on() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "{}\n").unwrap();
    let s = load_from(&path).unwrap();
    assert!(s.notification_sound);
}

#[test]
fn pre_existing_settings_file_without_notification_sound_key_still_loads() {
    // Simulates a settings.yaml written before `notification_sound`
    // existed: only `autostart` is present. Must not be rejected by
    // `deny_unknown_fields` (it isn't an unknown field, it's a
    // missing one) and must fall back to the field-level default.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "autostart: true\n").unwrap();
    let s = load_from(&path).unwrap();
    assert!(s.autostart);
    assert!(s.notification_sound);
}

#[test]
fn settings_path_uses_home() {
    // Avoid mutating $HOME globally — just assert the function
    // builds the path from $HOME deterministically when set.
    if let Some(home) = std::env::var_os("HOME") {
        let path = settings_path().unwrap();
        let expected = std::path::PathBuf::from(home)
            .join(".vet")
            .join("settings.yaml");
        assert_eq!(path, expected);
    }
}
