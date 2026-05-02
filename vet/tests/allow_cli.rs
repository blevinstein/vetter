//! CLI integration tests for `vet allow add | rm | list`.
//!
//! Each test gets its own tempdir; env (`XDG_CONFIG_HOME`, `HOME`)
//! and cwd are set per-spawn via `assert_cmd::Command`, so tests
//! parallelise without stomping on each other.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn vet() -> Command {
    Command::cargo_bin("vet").expect("vet binary")
}

/// Returns a base command + the scratch tempdir whose lifetime must
/// outlive the assertion (`Drop` deletes it). `XDG_CONFIG_HOME` is
/// pointed at the scratch dir so user-scope writes land there.
fn vet_isolated() -> (Command, TempDir) {
    let scratch = TempDir::new().expect("tempdir");
    let mut cmd = vet();
    cmd.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM")
        .env("XDG_CONFIG_HOME", scratch.path())
        .env_remove("HOME")
        .current_dir(scratch.path());
    (cmd, scratch)
}

fn user_allowlist_path(scratch: &TempDir) -> PathBuf {
    scratch.path().join("vet").join("allowlist.yaml")
}

const PATTERN_GET_EXAMPLE: &str =
    "{ when: { http: { method: [GET], url: { host: example.test } } } }";

#[test]
fn add_writes_to_user_scope_by_default() {
    let (mut cmd, scratch) = vet_isolated();
    let assertion = cmd
        .args(["allow", "add", PATTERN_GET_EXAMPLE])
        .assert()
        .success()
        .stdout(contains("added rule"))
        .stdout(contains("auto-"));
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("comments in the YAML are not preserved"),
        "missing round-trip warning: {stdout}"
    );
    let path = user_allowlist_path(&scratch);
    assert!(
        path.exists(),
        "user-scope file not written: {}",
        path.display()
    );
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("auto-"), "id missing from file: {body}");
    assert!(
        body.contains("example.test"),
        "host missing from file: {body}"
    );
}

#[test]
fn add_with_explicit_id_preserves_it() {
    let (mut cmd, scratch) = vet_isolated();
    cmd.args([
        "allow",
        "add",
        "{ id: my-rule, when: { http: { method: [GET], url: { host: example.test } } } }",
    ])
    .assert()
    .success()
    .stdout(contains("added rule `my-rule`"));
    let body = fs::read_to_string(user_allowlist_path(&scratch)).unwrap();
    assert!(body.contains("my-rule"));
}

#[test]
fn add_auto_id_is_stable_so_duplicate_add_errors() {
    let (mut cmd, _scratch) = vet_isolated();
    cmd.args(["allow", "add", PATTERN_GET_EXAMPLE])
        .assert()
        .success();

    let (mut cmd2, _scratch2) = (vet(), _scratch);
    cmd2.env_remove("HOME")
        .env("XDG_CONFIG_HOME", _scratch2.path())
        .current_dir(_scratch2.path())
        .args(["allow", "add", PATTERN_GET_EXAMPLE])
        .assert()
        .code(78)
        .stderr(contains("duplicate rule id"));
}

#[test]
fn add_rejects_malformed_yaml() {
    let (mut cmd, _scratch) = vet_isolated();
    cmd.args(["allow", "add", "{ this is: [not, valid"])
        .assert()
        .code(78)
        .stderr(contains("cannot parse pattern"));
}

#[test]
fn add_rejects_pattern_without_when() {
    let (mut cmd, _scratch) = vet_isolated();
    cmd.args(["allow", "add", "{ id: nope, when: {} }"])
        .assert()
        .code(78)
        .stderr(contains("non-empty `when:` clause"));
}

#[test]
fn add_with_allowlist_override_writes_to_that_file() {
    let (mut cmd, scratch) = vet_isolated();
    let target = scratch.path().join("custom.yaml");
    cmd.args([
        "--allowlist",
        target.to_str().unwrap(),
        "allow",
        "add",
        PATTERN_GET_EXAMPLE,
    ])
    .assert()
    .success();
    assert!(target.exists());
    assert!(!user_allowlist_path(&scratch).exists());
}

#[test]
fn add_scope_project_requires_repo_root() {
    let (mut cmd, _scratch) = vet_isolated();
    cmd.args(["allow", "add", "--scope", "project", PATTERN_GET_EXAMPLE])
        .assert()
        .code(78)
        .stderr(contains("no project root found"));
}

#[test]
fn add_scope_project_writes_under_dot_vet_at_repo_root() {
    let (mut cmd, scratch) = vet_isolated();
    fs::create_dir_all(scratch.path().join(".git")).unwrap();
    let nested = scratch.path().join("src/feature");
    fs::create_dir_all(&nested).unwrap();

    cmd.current_dir(&nested)
        .args(["allow", "add", "--scope", "project", PATTERN_GET_EXAMPLE])
        .assert()
        .success();

    let project_path = scratch.path().join(".vet/allowlist.yaml");
    assert!(project_path.exists(), "project file not written");
}

#[test]
fn rm_removes_existing_rule_and_preserves_others() {
    let (mut cmd, scratch) = vet_isolated();
    let path = user_allowlist_path(&scratch);
    seed_file(
        &path,
        r#"
rules:
  - id: keep-me
    when:
      http: { method: [GET] }
  - id: drop-me
    when:
      http: { method: [HEAD] }
deny:
  - id: blocked
    when:
      http: { method: [POST] }
"#,
    );
    cmd.args(["allow", "rm", "drop-me"])
        .assert()
        .success()
        .stdout(contains("removed rule `drop-me`"));
    let body = fs::read_to_string(&path).unwrap();
    assert!(!body.contains("drop-me"), "drop-me still present: {body}");
    assert!(body.contains("keep-me"), "keep-me lost: {body}");
    assert!(body.contains("blocked"), "denylist lost: {body}");
}

#[test]
fn rm_unknown_id_exits_78() {
    let (mut cmd, scratch) = vet_isolated();
    seed_file(
        &user_allowlist_path(&scratch),
        r#"
rules:
  - id: only
    when:
      http: { method: [GET] }
"#,
    );
    cmd.args(["allow", "rm", "ghost"])
        .assert()
        .code(78)
        .stderr(contains("no rule with id `ghost`"));
}

#[test]
fn rm_with_allowlist_override_targets_that_file() {
    let (mut cmd, scratch) = vet_isolated();
    let target = scratch.path().join("custom.yaml");
    seed_file(
        &target,
        r#"
rules:
  - id: x
    when:
      http: { method: [GET] }
"#,
    );
    cmd.args(["--allowlist", target.to_str().unwrap(), "allow", "rm", "x"])
        .assert()
        .success();
    let body = fs::read_to_string(&target).unwrap();
    assert!(!body.contains("- id: x"), "rule still present: {body}");
}

#[test]
fn list_prints_rules_grouped_by_scope() {
    let (mut cmd, scratch) = vet_isolated();
    seed_file(
        &user_allowlist_path(&scratch),
        r#"
rules:
  - id: from-user
    when:
      http: { method: [GET], url: { host: example.test } }
deny:
  - id: blocked
    when:
      http: { method: [POST], url: { host: bad.test } }
"#,
    );
    fs::create_dir_all(scratch.path().join(".vet")).unwrap();
    seed_file(
        &scratch.path().join(".vet/allowlist.yaml"),
        r#"
rules:
  - id: from-project
    when:
      http: { method: [HEAD], url: { host: project.test } }
"#,
    );
    let assertion = cmd.args(["allow", "list"]).assert().success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("[denylist] blocked"), "{stdout}");
    assert!(stdout.contains("[project] from-project"), "{stdout}");
    assert!(stdout.contains("[user] from-user"), "{stdout}");
    assert!(
        stdout.contains("example.test"),
        "summary missing host: {stdout}"
    );
}

#[test]
fn list_scope_user_filters_to_user_layer() {
    let (mut cmd, scratch) = vet_isolated();
    seed_file(
        &user_allowlist_path(&scratch),
        r#"
rules:
  - id: from-user
    when:
      http: { method: [GET] }
"#,
    );
    fs::create_dir_all(scratch.path().join(".vet")).unwrap();
    seed_file(
        &scratch.path().join(".vet/allowlist.yaml"),
        r#"
rules:
  - id: from-project
    when:
      http: { method: [HEAD] }
"#,
    );
    let assertion = cmd
        .args(["allow", "list", "--scope", "user"])
        .assert()
        .success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("from-user"), "{stdout}");
    assert!(!stdout.contains("from-project"), "project leaked: {stdout}");
}

#[test]
fn list_scope_project_filters_to_project_and_denylist() {
    let (mut cmd, scratch) = vet_isolated();
    seed_file(
        &user_allowlist_path(&scratch),
        r#"
rules:
  - id: from-user
    when:
      http: { method: [GET] }
"#,
    );
    fs::create_dir_all(scratch.path().join(".vet")).unwrap();
    seed_file(
        &scratch.path().join(".vet/allowlist.yaml"),
        r#"
rules:
  - id: from-project
    when:
      http: { method: [HEAD] }
deny:
  - id: blocked
    when:
      http: { method: [POST] }
"#,
    );
    let assertion = cmd
        .args(["allow", "list", "--scope", "project"])
        .assert()
        .success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("from-project"), "{stdout}");
    assert!(stdout.contains("blocked"), "denylist missing: {stdout}");
    assert!(!stdout.contains("from-user"), "user leaked: {stdout}");
}

#[test]
fn list_history_defers_with_phase3_message() {
    let (mut cmd, _scratch) = vet_isolated();
    cmd.args(["allow", "list", "--history"])
        .assert()
        .success()
        .stdout(contains("--history requires the daemon"))
        .stdout(contains("Phase 3"));
}

#[test]
fn list_with_no_rules_says_so() {
    let (mut cmd, _scratch) = vet_isolated();
    cmd.args(["allow", "list"])
        .assert()
        .success()
        .stdout(contains("no rules loaded"));
}

fn seed_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}
