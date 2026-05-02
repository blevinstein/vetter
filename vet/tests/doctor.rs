//! Phase 0 smoke test for `vet doctor`. Asserts the binary runs, exits 0,
//! and produces output that names each Phase 0 stubbed check.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn doctor_runs_and_reports_stubs() {
    Command::cargo_bin("vet")
        .expect("vet binary should be built")
        .arg("doctor")
        .assert()
        .success()
        .stdout(contains("daemon"))
        .stdout(contains("socket"))
        .stdout(contains("parsers registered"))
        .stdout(contains("code signing"));
}

#[test]
fn unimplemented_subcommand_exits_78() {
    Command::cargo_bin("vet")
        .expect("vet binary should be built")
        .args(["allow", "list"])
        .assert()
        .code(78)
        .stderr(contains("not implemented"));
}
