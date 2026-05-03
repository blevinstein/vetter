//! End-to-end refusal tests for the argv0 inode resolver.
//!
//! Closes ThreatModel.md T4 at the binary surface: `vet --explain
//! <some-path>` must refuse when the resolved binary is not the real
//! parser-target. We use `--explain` rather than wrap mode so the test
//! does not need a daemon.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn vet_for(scratch: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("vet").expect("vet binary should be built");
    cmd.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM")
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", scratch.path())
        .current_dir(scratch.path());
    cmd
}

/// Drop a copy of `/bin/sh` into `dir` under `name` and chmod it
/// executable. macOS SIP forbids `link(2)` against `/bin/sh` itself,
/// so callers stage a copy first and hardlink within `dir` from there.
fn place_sh_copy(dir: &Path, name: &str) -> PathBuf {
    let dst = dir.join(name);
    std::fs::copy("/bin/sh", &dst).expect("copy /bin/sh");
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).expect("chmod copy");
    dst
}

#[test]
fn explain_refuses_hardlinked_curl_pointing_at_sh() {
    // Stage a copy of /bin/sh in the scratch dir, then hardlink it
    // into a sibling `curl`. Same `(dev, ino)` as the staged sh, NOT
    // matching the real `/usr/bin/curl` on PATH, and the scratch dir
    // is not in the trusted-install-dir set → resolver rejects with
    // UntrustedBinary.
    let scratch = TempDir::new().expect("tempdir");
    let stage = place_sh_copy(scratch.path(), "sh-stage");
    let bad_curl = scratch.path().join("curl");
    std::fs::hard_link(&stage, &bad_curl).expect("hard_link bad curl");

    vet_for(&scratch)
        .args([
            "--explain",
            bad_curl.to_str().unwrap(),
            "https://example.test/",
        ])
        .assert()
        .code(78)
        .stderr(contains("cannot route"))
        .stderr(contains("resolves to"))
        .stderr(contains("trusted install dir"))
        .stderr(contains("ThreatModel.md T4"));
}

#[test]
fn explain_refuses_symlinked_curl_pointing_at_sh() {
    // Symlink follows during canonicalize → basename becomes
    // `sh-stage` → dispatch fails with NoParser. Different error
    // variant from the hardlink case, same outcome (exit 78, never
    // touch the curl parser).
    let scratch = TempDir::new().expect("tempdir");
    let stage = place_sh_copy(scratch.path(), "sh-stage");
    let bad_curl = scratch.path().join("curl");
    std::os::unix::fs::symlink(&stage, &bad_curl).expect("symlink bad curl");

    vet_for(&scratch)
        .args([
            "--explain",
            bad_curl.to_str().unwrap(),
            "https://example.test/",
        ])
        .assert()
        .code(78)
        .stderr(contains("cannot route"))
        .stderr(contains("no parser registered for `sh-stage`"));
}

#[test]
fn explain_accepts_bad_curl_when_dir_is_trusted_via_env() {
    // Same `cp /bin/sh -> tmp/curl` setup as the hardlink test, but
    // with `VETTER_PARSER_TRUSTED_DIRS=tmp` set. The (b) path-list
    // arm accepts the binary, so the resolver returns Ok and the
    // curl parser is invoked — which then chokes on `https://...` as
    // a non-curl-flag argv. We assert the resolver did NOT block it
    // (no "cannot route" line); the parser-level error is fine.
    let scratch = TempDir::new().expect("tempdir");
    let bad_curl = place_sh_copy(scratch.path(), "curl");
    let trusted = std::fs::canonicalize(scratch.path()).expect("canonicalize tmp");

    let assertion = vet_for(&scratch)
        .env("VETTER_PARSER_TRUSTED_DIRS", &trusted)
        .args([
            "--explain",
            bad_curl.to_str().unwrap(),
            "https://example.test/",
        ])
        .assert();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).expect("utf-8");
    assert!(
        !stderr.contains("cannot route"),
        "trusted-dir extension should let the resolver accept; stderr was: {stderr}"
    );
}
