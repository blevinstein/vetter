//! Smoke tests for `vet doctor`. After Phase 1b, the curl parser is
//! registered into release builds, so the parsers-registered count is
//! `1` and the names list mentions `curl`.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn doctor_runs_and_reports_stubs() {
    Command::cargo_bin("vet")
        .expect("vet binary should be built")
        .env("VETTERD_SOCKET", "/tmp/vetter-doctor-nonexistent.sock")
        .arg("doctor")
        .assert()
        .success()
        .stdout(contains("daemon"))
        .stdout(contains("not reachable"))
        .stdout(contains("socket"))
        .stdout(contains("parsers registered . 1"))
        .stdout(contains("curl"))
        .stdout(contains("code signing"));
}

#[test]
fn unimplemented_subcommand_exits_78() {
    // `vet daemon start` remains a Phase-3b stub; supervisor work
    // lands there. Once it does, swap this for any remaining
    // not-yet-implemented surface.
    Command::cargo_bin("vet")
        .expect("vet binary should be built")
        .args(["daemon", "start"])
        .assert()
        .code(78)
        .stderr(contains("not implemented"));
}
