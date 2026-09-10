//! Helpers for tests that mutate process-global environment.
//!
//! Hidden from the public docs. Downstream crates (`vetterd`) import
//! these so `$TMPDIR` overrides stay consistent across the workspace.

use std::path::PathBuf;

/// Real directory used when a test overrides `$TMPDIR`.
///
/// Must exist on disk: other modules call `TempDir::new()` in parallel
/// and that reads `$TMPDIR`. Pointing the override at a fictional path
/// (`/some/tmp`) makes those calls fail with `NotFound`.
///
/// Deliberately not a `tempfile::TempDir`: dropping one would delete
/// siblings that raced in under the override.
pub fn sticky_tmpdir() -> PathBuf {
    let dir = PathBuf::from(format!("/tmp/vetter-test-tmpdir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create sticky test TMPDIR");
    dir
}
