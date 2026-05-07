//! `vet curl ...` integration tests — covers the full thin-client
//! flow against a real `vetterd`. Each test gets its own daemon and
//! its own sentinel curl shim.

mod common;

use predicates::str::contains;

use common::{install_fake_curl, install_fake_curl_with_stdout, vet_cmd, Daemon, MockResponse};

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

/// Phase 4: `--dry-run` now routes to the notifier. The mock here
/// rejects, so the wire response is Deny and `vet` exits 77.
#[test]
fn dry_run_routes_to_notifier_and_can_be_rejected() {
    let d = Daemon::spawn(ALLOWLIST);
    d.ui.set_default(MockResponse::deny("test rejected dry-run"));
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    vet_cmd(&d.socket, dir.path())
        .args(["--dry-run", "curl", "https://example.test/"])
        .assert()
        .code(77)
        .stderr(contains("vet: deny"))
        .stderr(contains("rejected dry-run"));

    assert!(!marker.exists(), "dry-run must not exec");
}

/// Phase 4: a no-rule-match request now routes to the notifier; the
/// mock approves and the wrapped command runs.
#[test]
fn no_rule_match_routes_to_notifier_and_can_be_approved() {
    let d = Daemon::spawn(ALLOWLIST);
    d.ui.set_default(MockResponse::allow("test approved unmatched"));
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    install_fake_curl(dir.path(), &marker);

    vet_cmd(&d.socket, dir.path())
        .args(["curl", "https://unmatched.test/"])
        .assert()
        .success()
        .stderr(contains("vet: allow"))
        .stderr(contains("approved unmatched"));

    assert!(marker.exists(), "approved prompt must exec");
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

/// Phase 5.1 stdout-discipline guarantee: on the allow / exec path,
/// `vet` writes nothing of its own to stdout. The wrapped command's
/// stdout must reach the caller byte-perfect so pipelines like
/// `vet curl https://api/foo | jq .` work unmodified. Anything `vet`
/// itself wrote to fd 1 would corrupt this comparison.
#[test]
fn allow_path_passes_through_curl_stdout_unchanged() {
    let d = Daemon::spawn(ALLOWLIST);
    let dir = scratch();
    let marker = dir.path().join("ran.marker");
    let sentinel = "RESPONSE-BODY-FROM-FAKE-CURL\n";
    install_fake_curl_with_stdout(dir.path(), &marker, sentinel);

    let assertion = vet_cmd(&d.socket, dir.path())
        .args(["curl", "https://example.test/"])
        .assert()
        .success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    assert_eq!(
        stdout, sentinel,
        "vet leaked extra bytes to stdout on the allow / exec path: {stdout:?}"
    );

    assert!(
        marker.exists(),
        "fake curl marker missing — exec did not happen"
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
