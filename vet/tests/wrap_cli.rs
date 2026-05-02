//! `vet curl ...` integration tests — covers the full thin-client
//! flow against a real `vetterd`. Each test gets its own daemon and
//! its own sentinel curl shim.

mod common;

use predicates::str::contains;

use common::{install_fake_curl, vet_cmd, Daemon};

const ALLOWLIST: &str = r#"
rules:
  - id: example-get
    when:
      http:
        method: [GET]
        url:
          scheme: https
          host: example.test
        headers_allow: ["*"]
deny:
  - id: blocked-host
    when:
      http:
        method: [GET]
        url:
          scheme: https
          host: blocked.test
        headers_allow: ["*"]
"#;

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn allow_path_execs_wrapped_command() {
    let d = Daemon::spawn(ALLOWLIST);
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    vet_cmd(&d.socket, dir.path())
        .args(["curl", "https://example.test/"])
        .assert()
        .success()
        .stderr(contains("vet: allow"))
        .stderr(contains("example-get"));

    assert!(
        marker.exists(),
        "fake curl marker missing — exec did not happen"
    );
}

#[test]
fn deny_path_does_not_exec() {
    let d = Daemon::spawn(ALLOWLIST);
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    vet_cmd(&d.socket, dir.path())
        .args(["curl", "https://blocked.test/"])
        .assert()
        .code(77)
        .stderr(contains("vet: deny"))
        .stderr(contains("denylist"))
        .stderr(contains("blocked-host"));

    assert!(
        !marker.exists(),
        "deny path must not exec the wrapped command"
    );
}

#[test]
fn dry_run_forces_stub_deny_even_when_a_rule_would_allow() {
    let d = Daemon::spawn(ALLOWLIST);
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    vet_cmd(&d.socket, dir.path())
        .args(["--dry-run", "curl", "https://example.test/"])
        .assert()
        .code(77)
        .stderr(contains("no UI yet"));

    assert!(!marker.exists(), "dry-run must not exec");
}

#[test]
fn no_rule_match_falls_back_to_stub_deny() {
    let d = Daemon::spawn(ALLOWLIST);
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    vet_cmd(&d.socket, dir.path())
        .args(["curl", "https://unmatched.test/"])
        .assert()
        .code(77)
        .stderr(contains("no UI yet"));

    assert!(!marker.exists());
}

#[test]
fn quiet_suppresses_render_block_but_still_prints_outcome() {
    let d = Daemon::spawn(ALLOWLIST);
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    let assertion = vet_cmd(&d.socket, dir.path())
        .args(["--quiet", "curl", "https://example.test/"])
        .assert()
        .success();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).unwrap();
    // The renderer prints a horizontal-rule line `---...`. With
    // --quiet we don't expect any of those.
    assert!(
        !stderr.contains("───") && !stderr.contains("---"),
        "render block leaked through --quiet: {stderr:?}"
    );
    assert!(
        stderr.contains("vet: allow"),
        "outcome line missing: {stderr:?}"
    );
}

#[test]
fn render_goes_to_stderr_not_stdout_on_deny() {
    let d = Daemon::spawn(ALLOWLIST);
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    let assertion = vet_cmd(&d.socket, dir.path())
        .args(["curl", "https://blocked.test/"])
        .assert()
        .code(77);
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.is_empty(),
        "stdout must be passthrough-empty on deny: {stdout:?}"
    );
}
