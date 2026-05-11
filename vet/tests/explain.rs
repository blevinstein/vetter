//! CLI integration tests for `vet --explain` (Phase 1b).
//!
//! Spawn the real `vet` binary via `assert_cmd` so we exercise the full
//! clap → register → dispatch → parse → analyze → render pipeline. All
//! assertions are stable text patterns from the §8.5 layout and the
//! Phase 1b explain-mode policy line.

use std::os::unix::fs::PermissionsExt as _;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

fn vet() -> Command {
    Command::cargo_bin("vet").expect("vet binary should be built")
}

/// Strip env vars that could perturb color or allowlist discovery so
/// every test has a deterministic baseline. Tests that care about a
/// specific env re-set it explicitly. The tempdir is returned so its
/// lifetime extends over the assertion (Drop deletes the tempdir).
fn vet_nocolor_clean() -> (Command, TempDir) {
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
fn explain_curl_renders_to_stderr() {
    let (mut cmd, _scratch) = vet_nocolor_clean();
    cmd.args(["--explain", "curl", "https://example.test/foo"])
        .assert()
        .success()
        .stderr(contains("vet  curl"))
        .stderr(contains("GET"))
        .stderr(contains("https://example.test/foo"))
        .stderr(contains("Match:"))
        .stderr(contains("decision = prompt"));
}

#[test]
fn explain_quiet_suppresses_render_block_keeps_policy_line() {
    let (mut cmd, _scratch) = vet_nocolor_clean();
    let assertion = cmd
        .args(["--explain", "--quiet", "curl", "https://example.test/quiet"])
        .assert()
        .success()
        .stderr(contains("decision = prompt"));
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
    // `definitely-not-a-tool` is neither on $PATH nor a path on disk,
    // so the argv0 resolver bails with NotFound before dispatch.
    let (mut cmd, _scratch) = vet_nocolor_clean();
    cmd.args(["--explain", "definitely-not-a-tool", "anything"])
        .assert()
        .code(78)
        .stderr(contains("not found on $PATH"));
}

#[test]
fn explain_known_path_without_parser_exits_78() {
    // /bin/sh exists, is executable, and resolves cleanly — but no
    // parser is registered for `sh`, so dispatch reports NoParser.
    let (mut cmd, _scratch) = vet_nocolor_clean();
    cmd.args(["--explain", "/bin/sh", "-c", "true"])
        .assert()
        .code(78)
        .stderr(contains("no parser registered"));
}

#[test]
fn explain_streaming_unsupported_exits_78() {
    let (mut cmd, _scratch) = vet_nocolor_clean();
    cmd.args([
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
    let (mut cmd, _scratch) = vet_nocolor_clean();
    cmd.args(["--explain", "curl", "-k"])
        .assert()
        .code(78)
        .stderr(contains("missing required argument"));
}

#[test]
fn explain_no_color_strips_ansi_even_with_clicolor_force() {
    let (mut cmd, _scratch) = vet_nocolor_clean();
    let assertion = cmd
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
    let (mut cmd, _scratch) = vet_nocolor_clean();
    let assertion = cmd
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
    let (mut cmd, _scratch) = vet_nocolor_clean();
    let assertion = cmd
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
    let (mut cmd, _scratch) = vet_nocolor_clean();
    let assertion = cmd
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
fn non_explain_wrap_fails_closed_without_daemon() {
    // Phase 3a wires wrap mode through to the daemon. With no daemon
    // listening (we point the socket at a guaranteed-missing path),
    // the thin client must fail-closed: exit 78, never exec.
    let (mut cmd, scratch) = vet_nocolor_clean();
    let sock_dir = scratch.path().join("run");
    std::fs::create_dir(&sock_dir).unwrap();
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = sock_dir.join("missing.sock");
    cmd.env("VETTERD_SOCKET", &socket)
        .args(["curl", "https://example.test/"])
        .assert()
        .code(78)
        .stderr(contains("vetterd is not reachable"));
}

#[test]
fn dry_run_without_daemon_also_fails_closed() {
    // --dry-run still has to talk to the daemon (it's the daemon
    // that interprets force_prompt). With no daemon, fail-closed.
    let (mut cmd, scratch) = vet_nocolor_clean();
    let sock_dir = scratch.path().join("run");
    std::fs::create_dir(&sock_dir).unwrap();
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = sock_dir.join("missing.sock");
    cmd.env("VETTERD_SOCKET", &socket)
        .args(["--dry-run", "curl", "https://example.test/"])
        .assert()
        .code(78)
        .stderr(contains("vetterd is not reachable"));
}

/// Phase 5.1 stdout-discipline guarantee: `--explain` is read-only and
/// must keep stdout pristine so it composes with shell pipelines.
#[test]
fn explain_happy_path_emits_nothing_to_stdout() {
    let (mut cmd, _scratch) = vet_nocolor_clean();
    let assertion = cmd
        .args(["--explain", "curl", "https://example.test/"])
        .assert()
        .success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).expect("utf-8");
    assert!(
        stdout.is_empty(),
        "explain mode leaked bytes to stdout: {stdout:?}"
    );
}

#[test]
fn explain_parse_error_emits_nothing_to_stdout() {
    let (mut cmd, _scratch) = vet_nocolor_clean();
    let assertion = cmd.args(["--explain", "curl", "-k"]).assert().code(78);
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).expect("utf-8");
    assert!(
        stdout.is_empty(),
        "explain parse-error path leaked to stdout: {stdout:?}"
    );
}

#[test]
fn non_explain_wrap_no_daemon_emits_nothing_to_stdout() {
    let (mut cmd, scratch) = vet_nocolor_clean();
    let sock_dir = scratch.path().join("run");
    std::fs::create_dir(&sock_dir).unwrap();
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = sock_dir.join("missing.sock");
    let assertion = cmd
        .env("VETTERD_SOCKET", &socket)
        .args(["curl", "https://example.test/"])
        .assert()
        .code(78);
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).expect("utf-8");
    assert!(
        stdout.is_empty(),
        "fail-closed (no daemon) path leaked to stdout: {stdout:?}"
    );
}
