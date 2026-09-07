//! Tests for [`crate::runloop::model`]. Layout convention from `AGENTS.md`.
//!
//! Everything here runs with no display, no session bus and no main
//! loop — which is the point of keeping the lowering separate from
//! the widget assembly, since CI has neither.

use super::*;

use vetter_core::wire::WireDecision;

/// Minimal pending summary. Fields the window reads are set
/// explicitly by each test; the rest stay at their empty defaults.
fn summary(id: &str, command: &str, verb: &str, target: &str) -> PromptSummary {
    PromptSummary {
        id: id.into(),
        command: command.into(),
        primary_verb: verb.into(),
        primary_target: target.into(),
        force_prompt: false,
        signals: Vec::new(),
        parsed: None,
        host_known: Vec::new(),
        peer_sid: None,
    }
}

#[test]
fn approve_and_reject_carry_the_documented_audit_reasons() {
    // `plans/LinuxApp.md` §7 step 13 expects an audit log that
    // interleaves "approved via window" with the admin-socket and
    // notification forms, so an operator can tell the surfaces apart
    // after the fact. These strings are load-bearing, not cosmetic.
    let approve = CardAction::Approve.decision();
    assert_eq!(approve.decision, WireDecision::Allow);
    assert_eq!(approve.reason, "approved via window");

    let reject = CardAction::Reject.decision();
    assert_eq!(reject.decision, WireDecision::Deny);
    assert_eq!(reject.reason, "rejected via window");
}

#[test]
fn each_surface_uses_a_distinct_audit_reason() {
    // The whole point of the per-surface strings is telling them
    // apart; if two ever collided the audit log would silently stop
    // distinguishing who approved what.
    let window = [REASON_APPROVED, REASON_REJECTED];
    for other in [
        "approved via admin socket",
        "rejected via admin socket",
        "approved via notification",
        "rejected via notification",
    ] {
        assert!(
            !window.contains(&other),
            "window reason collides with `{other}`"
        );
    }
}

#[test]
fn card_shows_verb_and_target_together() {
    let card = card_view(&summary("01A", "curl", "GET", "https://example.com/"));
    assert_eq!(card.title, "curl");
    assert_eq!(card.target, "GET https://example.com/");
    assert_eq!(card.id, "01A");
}

#[test]
fn card_omits_an_empty_verb_rather_than_leaving_a_gap() {
    let card = card_view(&summary("01A", "curl", "", "https://example.com/"));
    assert_eq!(
        card.target, "https://example.com/",
        "an empty verb must not leave a leading space"
    );
}

#[test]
fn dry_run_is_marked_in_the_title() {
    // A dry-run prompt would otherwise be indistinguishable from a
    // real one, and approving it has different meaning.
    let mut s = summary("01A", "curl", "GET", "https://example.com/");
    s.force_prompt = true;
    assert_eq!(card_view(&s).title, "curl (dry run)");
}

#[test]
fn argv_derived_text_is_sanitised_before_it_reaches_a_widget() {
    // A hostile URL carrying an RTLO override could otherwise reorder
    // what the human reads on the surface they approve from — the
    // target would render as something other than what gets executed.
    let card = card_view(&summary(
        "01A",
        "curl",
        "GET",
        "https://example.com/\u{202e}gnp.exe",
    ));
    assert!(
        !card.target.contains('\u{202e}'),
        "RTLO survived into the card: {:?}",
        card.target
    );
}

#[test]
fn sanitising_applies_to_the_command_name_too() {
    let card = card_view(&summary(
        "01A",
        "cu\u{200b}rl",
        "GET",
        "https://example.com/",
    ));
    assert!(
        !card.title.contains('\u{200b}'),
        "zero-width char survived into the title: {:?}",
        card.title
    );
}

#[test]
fn trust_defaults_to_unknown_without_a_parsed_command() {
    // Conservative direction: an unknown-host pill overstates risk,
    // a known-host pill would understate it. A summary with no
    // parsed effects must never claim the safer answer.
    let card = card_view(&summary("01A", "curl", "GET", "https://example.com/"));
    assert_eq!(card.trust, cards::url::HostTrust::Unknown);
}

#[test]
fn cards_are_ordered_oldest_first() {
    // ULIDs sort chronologically, so the longest-blocked request —
    // the one an agent has been waiting on — lands at the top where
    // it is reachable without scrolling.
    let pending = vec![
        summary("01C", "curl", "GET", "https://c.example/"),
        summary("01A", "curl", "GET", "https://a.example/"),
        summary("01B", "curl", "GET", "https://b.example/"),
    ];
    let ids: Vec<String> = snapshot(&pending).into_iter().map(|c| c.id).collect();
    assert_eq!(ids, ["01A", "01B", "01C"]);
}

#[test]
fn an_empty_queue_lowers_to_no_cards() {
    assert!(snapshot(&[]).is_empty());
}

#[test]
fn the_empty_state_points_at_the_other_surfaces() {
    // A user staring at an empty window on a box where the tray or
    // notifications are doing the work should learn where else a
    // decision can come from, rather than assuming vetter is idle.
    assert!(EMPTY_BODY.contains("vet daemon approve"));
    assert!(EMPTY_BODY.contains("tray"));
    assert!(EMPTY_BODY.contains("notification"));
}
