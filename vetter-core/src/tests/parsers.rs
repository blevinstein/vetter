//! Tests for [`crate::parsers`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;

use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;

#[test]
fn basename_of_strips_directory() {
    assert_eq!(basename_of("/opt/homebrew/bin/curl"), "curl");
    assert_eq!(basename_of("curl"), "curl");
    assert_eq!(basename_of("./curl"), "curl");
    assert_eq!(basename_of(""), "");
}

#[test]
fn parse_error_messages_are_human_readable() {
    let e = ParseError::MissingArgument("URL".into()).to_string();
    assert!(e.contains("URL"), "{e}");
    let s = ParseError::StreamingUnsupported.to_string();
    assert!(s.contains("streaming"), "{s}");
}

// --- argv0 inode resolution (ThreatModel.md T4) ----------------------------

/// Ensures the `noop` parser is registered exactly once across the lib's
/// test binary. `register()` panics on duplicate, so each `#[test]`
/// calls this helper instead of registering directly.
fn ensure_noop_registered() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        register(Box::new(crate::parsers::noop::NoopParser));
    });
}

/// Build a `ResolveEnv` with PATH pointing at `path_dirs` (joined with
/// `:`) and `VETTER_PARSER_TRUSTED_DIRS` set to `trusted_dirs` (also
/// `:`-joined). Either may be empty to omit.
fn env_with(path_dirs: &[&Path], trusted_dirs: &[&Path]) -> ResolveEnv {
    fn join(parts: &[&Path]) -> Option<OsString> {
        if parts.is_empty() {
            None
        } else {
            Some(std::env::join_paths(parts).expect("join_paths"))
        }
    }
    ResolveEnv {
        path: join(path_dirs),
        extra_trusted: join(trusted_dirs),
    }
}

/// Drop a copy of `/bin/sh` into `dir` under `name` and chmod it
/// executable. Returns the canonical path so callers can hardlink from
/// it without tripping macOS SIP (which forbids `link(2)` against
/// system files in `/bin`, `/usr/bin`, etc.).
fn place_sh_copy(dir: &Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dst = dir.join(name);
    std::fs::copy("/bin/sh", &dst).expect("copy /bin/sh");
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).expect("chmod copy");
    dst
}

#[test]
fn resolve_empty_argv_errors() {
    ensure_noop_registered();
    let env = env_with(&[], &[]);
    match resolve_with_env("", &env) {
        Err(ResolveError::EmptyArgv) => {}
        other => panic!("expected EmptyArgv, got {other:?}"),
    }
}

#[test]
fn resolve_bare_name_not_on_path_errors() {
    ensure_noop_registered();
    // PATH points at an empty tempdir so the bare name has nowhere
    // to resolve from.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = env_with(&[tmp.path()], &[]);
    match resolve_with_env("definitely-not-a-real-binary", &env) {
        Err(ResolveError::NotFound(name)) => {
            assert_eq!(name, "definitely-not-a-real-binary");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn resolve_unknown_command_errors_when_no_parser_registered() {
    ensure_noop_registered();
    // Place a real executable under a name no parser claims. Resolve
    // reaches the dispatch step and bails with NoParser.
    let tmp = tempfile::tempdir().expect("tempdir");
    let foreign = place_sh_copy(tmp.path(), "not-a-known-parser");
    let env = env_with(&[], &[]);

    match resolve_with_env(foreign.to_str().unwrap(), &env) {
        Err(ResolveError::NoParser(name)) => {
            assert_eq!(name, "not-a-known-parser");
        }
        other => panic!("expected NoParser, got {other:?}"),
    }
}

#[test]
fn resolve_hardlink_to_other_binary_is_rejected() {
    ensure_noop_registered();
    // Stage a sh-like binary at tmp/sh-stage, then hardlink it into
    // tmp/noop. Same (dev, ino), but it's NOT the canonical "noop" on
    // PATH (PATH is empty here) and tmp is not in the trusted-dir
    // set, so both arms reject.
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = place_sh_copy(tmp.path(), "sh-stage");
    let bad = tmp.path().join("noop");
    std::fs::hard_link(&stage, &bad).expect("hard_link within tempdir");
    let env = env_with(&[], &[]);

    match resolve_with_env(bad.to_str().unwrap(), &env) {
        Err(ResolveError::UntrustedBinary {
            argv0,
            resolved,
            parser_name,
        }) => {
            assert_eq!(argv0, bad.to_str().unwrap());
            assert_eq!(parser_name, "noop");
            // Hardlink: canonical path stays at tmp/noop (canonicalize
            // does not follow hardlinks). Compare via canonicalize so
            // /private/tmp vs /tmp on macOS doesn't trip the assert.
            assert_eq!(resolved, std::fs::canonicalize(&bad).unwrap());
        }
        other => panic!("expected UntrustedBinary, got {other:?}"),
    }
}

#[test]
fn resolve_symlink_to_other_binary_is_rejected() {
    ensure_noop_registered();
    // Symlink follows during canonicalize → basename becomes the
    // staged sh-like name → dispatch fails with NoParser. That is
    // the cheapest catch and equally serves the threat model.
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = place_sh_copy(tmp.path(), "sh-stage");
    let bad = tmp.path().join("noop");
    std::os::unix::fs::symlink(&stage, &bad).expect("symlink");
    let env = env_with(&[], &[]);

    match resolve_with_env(bad.to_str().unwrap(), &env) {
        Err(ResolveError::NoParser(name)) => {
            assert_eq!(
                name, "sh-stage",
                "expected dispatch to bail on canonical basename"
            );
        }
        other => panic!("expected NoParser, got {other:?}"),
    }
}

#[test]
fn resolve_copy_of_sh_is_rejected_outside_trusted_dirs() {
    ensure_noop_registered();
    // Independent file, not in any trusted dir, no matching binary
    // on PATH → UntrustedBinary.
    let tmp = tempfile::tempdir().expect("tempdir");
    let bad = place_sh_copy(tmp.path(), "noop");
    let env = env_with(&[], &[]);

    match resolve_with_env(bad.to_str().unwrap(), &env) {
        Err(ResolveError::UntrustedBinary { parser_name, .. }) => {
            assert_eq!(parser_name, "noop");
        }
        other => panic!("expected UntrustedBinary, got {other:?}"),
    }
}

#[test]
fn resolve_trusted_dirs_env_extension_accepts() {
    ensure_noop_registered();
    // Same copy-of-sh as the previous test, but with the parent dir
    // listed in `VETTER_PARSER_TRUSTED_DIRS`. The (b) path-list arm
    // accepts even though the inode arm cannot.
    let tmp = tempfile::tempdir().expect("tempdir");
    let bad = place_sh_copy(tmp.path(), "noop");

    // Canonicalise the trusted dir we hand the resolver — the test
    // tempdir on macOS is `/var/folders/...` symlinked from
    // `/private/var/folders/...`; the resolver canonicalises both
    // sides before comparing, but we want the test to pass even
    // when the symlink behaviour shifts.
    let trusted = std::fs::canonicalize(tmp.path()).expect("canonicalize tmp");
    let env = env_with(&[], &[trusted.as_path()]);

    let resolved = resolve_with_env(bad.to_str().unwrap(), &env)
        .expect("trusted-dir extension should accept the copy");
    assert_eq!(resolved.parser.name(), "noop");
    assert_eq!(resolved.resolved_path, std::fs::canonicalize(&bad).unwrap());
}

#[test]
fn resolve_inode_match_via_path_accepts() {
    ensure_noop_registered();
    // tmp/noop is a hardlink to a staged sh-like binary in the same
    // tempdir, and PATH points at the same tempdir. `which("noop")`
    // therefore returns tmp/noop itself — same (dev, ino) as the
    // resolved argv0 — so the (a) inode arm accepts even though the
    // parent dir is not in the trusted-install set.
    let tmp = tempfile::tempdir().expect("tempdir");
    let stage = place_sh_copy(tmp.path(), "sh-stage");
    let target = tmp.path().join("noop");
    std::fs::hard_link(&stage, &target).expect("hard_link argv0");
    let env = env_with(&[tmp.path()], &[]);

    let resolved =
        resolve_with_env(target.to_str().unwrap(), &env).expect("inode arm should accept");
    assert_eq!(resolved.parser.name(), "noop");
}
