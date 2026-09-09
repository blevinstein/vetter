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
