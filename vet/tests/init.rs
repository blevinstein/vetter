//! CLI integration tests for `vet init`.

use std::os::unix::fs::PermissionsExt as _;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn vet() -> Command {
    Command::cargo_bin("vet").expect("vet binary")
}

fn vet_isolated() -> (Command, TempDir) {
    let scratch = TempDir::new().expect("tempdir");
    let mut cmd = vet();
    cmd.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM")
        .env("HOME", scratch.path())
        .env_remove("XDG_CONFIG_HOME")
        .current_dir(scratch.path());
    (cmd, scratch)
}

#[test]
fn init_creates_config_files() {
    let (mut cmd, scratch) = vet_isolated();
    cmd.arg("init")
        .assert()
        .success()
        .stdout(contains("created:"))
        .stdout(contains("allowlist.yaml"))
        .stdout(contains("known-hosts.yaml"))
        .stdout(contains("settings.yaml"));

    let vet_dir = scratch.path().join(".vet");
    assert!(vet_dir.is_dir());
    let dir_mode = vet_dir.metadata().unwrap().permissions().mode() & 0o777;
    assert_eq!(
        dir_mode, 0o700,
        "dir mode should be 0700, got 0{dir_mode:o}"
    );

    for name in ["allowlist.yaml", "known-hosts.yaml", "settings.yaml"] {
        let path = vet_dir.join(name);
        assert!(path.exists(), "{name} should exist");
        let mode = path.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{name} mode should be 0600, got 0{mode:o}");
    }
}

#[test]
fn init_is_idempotent() {
    let (mut cmd, scratch) = vet_isolated();
    cmd.arg("init").assert().success();

    let allowlist_path = scratch.path().join(".vet").join("allowlist.yaml");
    let original = std::fs::read_to_string(&allowlist_path).unwrap();

    let mut cmd2 = Command::cargo_bin("vet").expect("vet binary");
    cmd2.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM")
        .env("HOME", scratch.path())
        .env_remove("XDG_CONFIG_HOME")
        .current_dir(scratch.path());
    cmd2.arg("init")
        .assert()
        .success()
        .stdout(contains("exists:"));

    let after = std::fs::read_to_string(&allowlist_path).unwrap();
    assert_eq!(original, after, "file should not be overwritten");
}

#[test]
fn init_fails_when_home_unset() {
    let mut cmd = vet();
    cmd.env_remove("HOME").arg("init").assert().code(78);
}
