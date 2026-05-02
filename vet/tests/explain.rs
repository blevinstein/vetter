//! CLI integration tests for `vet --explain` (Phase 1b).
//!
//! Spawn the real `vet` binary via `assert_cmd` so we exercise the full
//! clap → register → dispatch → parse → analyze → render pipeline. All
//! assertions are stable text patterns from the §8.5 layout and the
//! Phase 1b explain-mode policy line.

use assert_cmd::Command;
use predicates::str::contains;

fn vet() -> Command {
    Command::cargo_bin("vet").expect("vet binary should be built")
}

/// Strip every env var that could perturb color detection so each test
/// has a deterministic baseline. Tests that care about a specific env
/// re-set it explicitly.
fn vet_nocolor_clean() -> Command {
    let mut cmd = vet();
    cmd.env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("CLICOLOR")
        .env_remove("TERM");
    cmd
}

#[test]
fn explain_curl_renders_to_stderr() {
    vet_nocolor_clean()
        .args(["--explain", "curl", "https://example.test/foo"])
        .assert()
        .success()
        .stderr(contains("vet  curl"))
        .stderr(contains("GET"))
        .stderr(contains("https://example.test/foo"))
        .stderr(contains("Match:"))
        .stderr(contains("--explain mode"));
}

#[test]
fn explain_quiet_suppresses_render_block_keeps_policy_line() {
    let assertion = vet_nocolor_clean()
        .args(["--explain", "--quiet", "curl", "https://example.test/quiet"])
        .assert()
        .success()
        .stderr(contains("--explain mode"));
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).expect("utf-8");
    assert!(
        !stderr.contains("vet  curl"),
        "render block should be suppressed under --quiet, got: {stderr}"
    );
    assert!(
        !stderr.contains("Match:"),
        "render block should be suppressed under --quiet, got: {stderr}"
    );
}

#[test]
fn explain_unknown_command_exits_78() {
    vet_nocolor_clean()
        .args(["--explain", "definitely-not-a-tool", "anything"])
        .assert()
        .code(78)
        .stderr(contains("no parser"));
}

#[test]
fn explain_streaming_unsupported_exits_78() {
    vet_nocolor_clean()
        .args([
            "--explain",
            "curl",
            "-T",
            "-",
            "https://example.test/upload",
        ])
        .assert()
        .code(78)
        .stderr(contains("streaming"));
}

#[test]
fn explain_parse_error_missing_url_exits_78() {
    vet_nocolor_clean()
        .args(["--explain", "curl", "-k"])
        .assert()
        .code(78)
        .stderr(contains("missing required argument"));
}

#[test]
fn explain_no_color_strips_ansi_even_with_clicolor_force() {
    let assertion = vet_nocolor_clean()
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "1")
        .args(["--explain", "curl", "https://example.test/"])
        .assert()
        .success();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).expect("utf-8");
    assert!(
        !stderr.contains('\x1b'),
        "ANSI escape leaked into stderr under NO_COLOR=1: {stderr:?}"
    );
}

#[test]
fn explain_clicolor_force_emits_ansi_when_stderr_is_redirected() {
    let assertion = vet_nocolor_clean()
        .env("CLICOLOR_FORCE", "1")
        .args(["--explain", "curl", "https://example.test/"])
        .assert()
        .success();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).expect("utf-8");
    assert!(
        stderr.contains('\x1b'),
        "expected ANSI escape under CLICOLOR_FORCE=1, got: {stderr:?}"
    );
}

#[test]
fn explain_default_to_non_tty_pipe_yields_plain_output() {
    let assertion = vet_nocolor_clean()
        .args(["--explain", "curl", "https://example.test/"])
        .assert()
        .success();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).expect("utf-8");
    assert!(
        !stderr.contains('\x1b'),
        "stderr piped (non-TTY) should default to plain, got: {stderr:?}"
    );
}

#[test]
fn explain_redacts_bearer_token_in_render() {
    // Property echo of the §7 / §8.5 secret-redaction guarantee.
    let assertion = vet_nocolor_clean()
        .args([
            "--explain",
            "curl",
            "-H",
            "Authorization: Bearer tok-secret-do-not-leak",
            "https://example.test/",
        ])
        .assert()
        .success();
    let stderr = String::from_utf8(assertion.get_output().stderr.clone()).expect("utf-8");
    assert!(
        !stderr.contains("tok-secret-do-not-leak"),
        "raw bearer token leaked into render output: {stderr}"
    );
    assert!(stderr.contains("••••"), "missing redaction marker");
}

#[test]
fn non_explain_wrap_still_not_implemented() {
    vet_nocolor_clean()
        .args(["curl", "https://example.test/"])
        .assert()
        .code(78)
        .stderr(contains("not implemented"))
        .stderr(contains("Phase 3"));
}

#[test]
fn dry_run_routes_to_not_implemented() {
    vet_nocolor_clean()
        .args(["--dry-run", "curl", "https://example.test/"])
        .assert()
        .code(78)
        .stderr(contains("not implemented"));
}
