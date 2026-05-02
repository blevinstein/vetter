//! Shared helpers for `vetterd` unit tests. Layout convention is
//! described in `AGENTS.md`.

/// Create a fresh tempdir pinned to `/tmp` instead of `tempfile::tempdir()`.
///
/// `paths::tests` mutates the process-global `TMPDIR` env variable as
/// part of testing the fallback chain. `tempfile::tempdir()` honours
/// `TMPDIR`, which means a parallel `socket::tests` / `audit::tests`
/// could otherwise see a `TMPDIR` pointing at a non-existent path and
/// fail with `NotFound`. Pinning the base directory removes the race
/// without serialising the entire test binary.
pub(crate) fn tmpdir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .expect("create tempdir under /tmp")
}
