//! Smoke tests for `vet doctor`. After Phase 1b, the curl parser is
//! registered into release builds, so the parsers-registered count is
//! `1` and the names list mentions `curl`.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn doctor_runs_and_reports_stubs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("vetter-doctor-nonexistent.sock");
    let pidfile = dir.path().join("vetter-doctor-nonexistent.pid");
    Command::cargo_bin("vet")
        .expect("vet binary should be built")
        .env("VETTERD_SOCKET", &socket)
        .env("VETTERD_PIDFILE", &pidfile)
        .arg("doctor")
        .assert()
        .success()
        .stdout(contains("daemon"))
        .stdout(contains("not reachable"))
        .stdout(contains("socket"))
        .stdout(contains("pidfile"))
        .stdout(contains("absent"))
        .stdout(contains("parsers registered . 1"))
        .stdout(contains("curl"))
        .stdout(contains("code signing"));
}
