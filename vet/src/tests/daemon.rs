//! Tests for [`crate::daemon`]. Layout convention from `AGENTS.md`.

use super::*;

/// The regression this whole change exists for.
///
/// `vet daemon open` used to print "approval window raised"
/// unconditionally. On Wayland a client cannot raise or focus itself
/// unprompted, so presenting an already-mapped window behind another
/// one is a visible no-op — and the CLI was reporting it as success.
/// Only the outcome the daemon actually measured as focused may make
/// that claim.
#[test]
fn only_a_focused_raise_claims_the_window_was_raised() {
    assert!(raise_message(WindowRaise::Focused).contains("raised"));
    assert!(!raise_message(WindowRaise::Unfocused).contains("raised"));
    assert!(!raise_message(WindowRaise::Unknown).contains("raised"));
}

/// An unmeasured outcome must not be dressed up as either result.
/// `Unknown` is what a timed-out UI thread or an older daemon yields,
/// and hedging is the honest answer for both.
#[test]
fn unknown_asserts_neither_outcome() {
    let msg = raise_message(WindowRaise::Unknown);
    assert!(
        msg.contains("could not confirm"),
        "unknown must say so plainly: {msg}"
    );
}

/// When the compositor declines, the useful part is not the refusal
/// but where the window went — otherwise the user is told a
/// non-actionable fact about window management.
#[test]
fn unfocused_says_where_to_look() {
    let msg = raise_message(WindowRaise::Unfocused);
    assert!(msg.contains("showing"), "should confirm it is on screen");
    assert!(
        msg.contains("taskbar") || msg.contains("workspace"),
        "should say where to look: {msg}"
    );
}

/// Three outcomes, three messages. A collision would make two
/// genuinely different situations indistinguishable to the caller,
/// which is the failure mode being fixed.
#[test]
fn every_outcome_reads_differently() {
    let all = [
        raise_message(WindowRaise::Focused),
        raise_message(WindowRaise::Unfocused),
        raise_message(WindowRaise::Unknown),
    ];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(a, b, "outcomes must not share a message");
        }
    }
}

// ── vetterd discovery ───────────────────────────────────────────────────────

/// Build a directory holding a fake `vet`, optionally with `vetterd`
/// beside it. Returns the path to the fake `vet`.
fn fake_install(root: &Path, name: &str, with_vetterd: bool) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create install dir");
    let vet = dir.join("vet");
    std::fs::write(&vet, b"#!/bin/sh\n").expect("write vet");
    if with_vetterd {
        std::fs::write(dir.join("vetterd"), b"#!/bin/sh\n").expect("write vetterd");
    }
    vet
}

/// The ordinary case: Cargo `target/release`, or the Linux tarball's
/// `~/.local/bin`, where both binaries are real files side by side.
#[test]
fn finds_vetterd_beside_the_invoked_vet() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let vet = fake_install(tmp.path(), "bin", true);

    let found = vetterd_beside_vet(&vet).expect("vetterd sits right there");
    assert_eq!(found, vet.parent().unwrap().join("vetterd"));
}

/// The regression this change exists for: a Homebrew cask.
///
/// The cask links only `vet` into the brew prefix while both binaries
/// live inside `Vetter.app/Contents/MacOS/`. macOS `current_exe()`
/// hands back the unresolved link, so the as-invoked pass looks in a
/// directory that has no `vetterd` — and before this fix the lookup
/// gave up there, breaking `vet daemon start` on the primary macOS
/// install path.
#[test]
fn resolves_through_a_symlinked_vet_into_the_bundle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real_vet = fake_install(tmp.path(), "Vetter.app", true);

    // The brew prefix: a `vet` symlink and deliberately no `vetterd`.
    let prefix = tmp.path().join("prefix-bin");
    std::fs::create_dir_all(&prefix).expect("create prefix");
    let linked_vet = prefix.join("vet");
    std::os::unix::fs::symlink(&real_vet, &linked_vet).expect("symlink vet");
    assert!(
        !prefix.join("vetterd").exists(),
        "the whole point is that the invoked directory lacks vetterd"
    );

    let found = vetterd_beside_vet(&linked_vet).expect("resolve into the bundle");
    assert_eq!(found, real_vet.parent().unwrap().join("vetterd"));
}

/// The as-invoked directory is searched *first*, so no layout that
/// resolves today can regress. A package manager linking both binaries
/// into one bin directory from separate real locations must keep
/// getting the sibling it already gets, even though canonicalising
/// would send the two passes to different directories.
#[test]
fn the_invoked_directory_wins_over_the_resolved_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real_vet = fake_install(tmp.path(), "cellar", true);

    // Both binaries present in the bin dir, `vet` only as a link.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let linked_vet = bin.join("vet");
    std::os::unix::fs::symlink(&real_vet, &linked_vet).expect("symlink vet");
    std::fs::write(bin.join("vetterd"), b"#!/bin/sh\n").expect("write vetterd");

    let found = vetterd_beside_vet(&linked_vet).expect("sibling exists as invoked");
    assert_eq!(found, bin.join("vetterd"), "must not canonicalise past it");
}

/// Neither directory has it: the caller falls through to `PATH`.
#[test]
fn reports_nothing_when_no_directory_holds_vetterd() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real_vet = fake_install(tmp.path(), "app", false);

    let prefix = tmp.path().join("prefix-bin");
    std::fs::create_dir_all(&prefix).expect("create prefix");
    let linked_vet = prefix.join("vet");
    std::os::unix::fs::symlink(&real_vet, &linked_vet).expect("symlink vet");

    assert!(vetterd_beside_vet(&linked_vet).is_none());
    assert!(vetterd_beside_vet(&real_vet).is_none());
}

/// A dangling symlink must not panic or resolve to something stray;
/// `canonicalize` fails and the lookup simply reports nothing.
#[test]
fn a_dangling_vet_symlink_resolves_to_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let prefix = tmp.path().join("prefix-bin");
    std::fs::create_dir_all(&prefix).expect("create prefix");
    let linked_vet = prefix.join("vet");
    std::os::unix::fs::symlink(tmp.path().join("gone/vet"), &linked_vet).expect("symlink");

    assert!(vetterd_beside_vet(&linked_vet).is_none());
}

/// The failure text has to name every step actually taken, or a user
/// debugging a broken install is told to check the wrong places.
#[test]
fn the_not_found_message_names_every_step() {
    assert!(VETTERD_NOT_FOUND.contains("$VETTERD_BIN"));
    assert!(VETTERD_NOT_FOUND.contains("symlink"));
    assert!(VETTERD_NOT_FOUND.contains("PATH"));
}
