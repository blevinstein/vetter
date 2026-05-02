//! Phase 0 smoke test for `vetterd`. Asserts the binary runs and exits 0.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn vetterd_stub_runs_and_exits_zero() {
    Command::cargo_bin("vetterd")
        .expect("vetterd binary should be built")
        .assert()
        .success()
        .stdout(contains("Phase 0 stub"));
}
