//! End-to-end tests for `vet --explain --allowlist <fixture> ...`.
//!
//! Each test writes a small YAML allowlist to a tempdir and shells out
//! to the real `vet` binary. The assertions read stderr — `vet` always
//! exits `0` from `--explain`, regardless of the decision (per
//! `plans/TestingPlan.md` §4.2).

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use tempfile::TempDir;

fn write_fixture(dir: &TempDir, body: &str) -> PathBuf {
    let path = dir.path().join("allowlist.yaml");
    fs::write(&path, body).expect("write fixture");
    path
}

fn vet_with(scratch: &TempDir, fixture: &PathBuf, argv: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("vet").expect("vet binary");
    cmd.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM")
        .env("XDG_CONFIG_HOME", scratch.path())
        .env_remove("HOME")
        .current_dir(scratch.path())
        .arg("--explain")
        .arg("--allowlist")
        .arg(fixture)
        .args(argv);
    cmd
}

fn stderr_of(mut cmd: Command, expect_code: i32) -> String {
    let assertion = cmd.assert().code(expect_code);
    String::from_utf8(assertion.get_output().stderr.clone()).expect("utf-8")
}

const ALLOW_EXAMPLE_GET: &str = r#"
rules:
  - id: allow-example-get
    when:
      http:
        method: [GET]
        url:
          scheme: https
          host: example.test
          port: [443]
          path: "/v1/**"
        headers_allow: ["*"]
        no_body: true
"#;

#[test]
fn allow_path_reports_decision_and_match_line() {
    let scratch = TempDir::new().unwrap();
    let fixture = write_fixture(&scratch, ALLOW_EXAMPLE_GET);
    let cmd = vet_with(&scratch, &fixture, &["curl", "https://example.test/v1/foo"]);
    let stderr = stderr_of(cmd, 0);
    assert!(
        stderr.contains("matched rule allow-example-get"),
        "stderr did not contain match line: {stderr}"
    );
    assert!(
        stderr.contains("decision = allow"),
        "stderr did not contain decision line: {stderr}"
    );
}

#[test]
fn deny_path_reports_denylist() {
    let scratch = TempDir::new().unwrap();
    let fixture = write_fixture(
        &scratch,
        r#"
deny:
  - id: no-posts
    when:
      http:
        method: [POST]
        url:
          host: example.test
        headers_allow: ["*"]
"#,
    );
    let cmd = vet_with(
        &scratch,
        &fixture,
        &[
            "curl",
            "-X",
            "POST",
            "-d",
            "{}",
            "https://example.test/v1/foo",
        ],
    );
    let stderr = stderr_of(cmd, 0);
    assert!(
        stderr.contains("denylist no-posts"),
        "stderr missing deny line: {stderr}"
    );
    assert!(
        stderr.contains("decision = deny"),
        "stderr missing decision line: {stderr}"
    );
}

#[test]
fn prompt_path_reports_no_rule() {
    let scratch = TempDir::new().unwrap();
    let fixture = write_fixture(&scratch, "rules: []\n");
    let cmd = vet_with(&scratch, &fixture, &["curl", "https://example.test/"]);
    let stderr = stderr_of(cmd, 0);
    assert!(
        stderr.contains("no rule"),
        "stderr missing no-rule: {stderr}"
    );
    assert!(
        stderr.contains("decision = prompt"),
        "stderr missing prompt: {stderr}"
    );
}

#[test]
fn host_suffix_confusion_is_rejected() {
    let scratch = TempDir::new().unwrap();
    let fixture = write_fixture(
        &scratch,
        r#"
rules:
  - id: only-example
    when:
      http:
        method: [GET]
        url:
          host: example.test
        headers_allow: ["*"]
"#,
    );
    let cmd = vet_with(
        &scratch,
        &fixture,
        &["curl", "https://example.test.attacker.test/foo"],
    );
    let stderr = stderr_of(cmd, 0);
    assert!(
        stderr.contains("decision = prompt"),
        "host-suffix confusion should yield prompt, got: {stderr}"
    );
}

#[test]
fn path_traversal_is_normalised_then_rejected() {
    let scratch = TempDir::new().unwrap();
    let fixture = write_fixture(
        &scratch,
        r#"
rules:
  - id: only-admin
    when:
      http:
        method: [GET]
        url:
          host: example.test
          path: "/admin"
        headers_allow: ["*"]
"#,
    );
    let cmd = vet_with(
        &scratch,
        &fixture,
        &["curl", "https://example.test/admin/../secret"],
    );
    let stderr = stderr_of(cmd, 0);
    assert!(
        stderr.contains("decision = prompt"),
        "path traversal should yield prompt, got: {stderr}"
    );
}

#[test]
fn denylist_overrides_an_allowing_rule_in_same_file() {
    let scratch = TempDir::new().unwrap();
    let fixture = write_fixture(
        &scratch,
        r#"
rules:
  - id: allow-everything
    when:
      http:
        method: [GET, HEAD, POST]
        url:
          host: example.test
        headers_allow: ["*"]
deny:
  - id: block-secret
    when:
      http:
        method: [GET]
        url:
          host: example.test
          path: "/secret"
        headers_allow: ["*"]
"#,
    );
    let cmd = vet_with(&scratch, &fixture, &["curl", "https://example.test/secret"]);
    let stderr = stderr_of(cmd, 0);
    assert!(
        stderr.contains("denylist block-secret"),
        "denylist should win, got: {stderr}"
    );
    assert!(
        stderr.contains("decision = deny"),
        "decision should be deny, got: {stderr}"
    );
}

#[test]
fn malformed_yaml_yields_exit_78() {
    let scratch = TempDir::new().unwrap();
    let fixture = write_fixture(&scratch, "rules: [this is broken\n");
    let cmd = vet_with(&scratch, &fixture, &["curl", "https://example.test/"]);
    let stderr = stderr_of(cmd, 78);
    assert!(
        stderr.contains("allowlist load failed"),
        "stderr missing load-failed message: {stderr}"
    );
}
