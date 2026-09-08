//! Tests for [`crate::cards::resolved`]. Layout convention from
//! `AGENTS.md`.

use super::*;

use crate::testutil::summary;

// ── Resolved cards ──────────────────────────────────────────────────────────

fn resolved(
    id: &str,
    decision: WireDecision,
    rule_id: Option<&str>,
    rule_scope: Option<Scope>,
) -> ResolvedEntry {
    ResolvedEntry {
        summary: summary(id, "curl", "GET", "https://a.example/"),
        rendered: String::new(),
        decision,
        rule_id: rule_id.map(str::to_string),
        rule_scope,
    }
}

#[test]
fn a_denied_card_offers_no_pickers() {
    // Surfacing "Allowlist…" straight after someone pressed Reject
    // reads as arguing with them, and a rule added from a denied card
    // would contradict the decision the user just made.
    let card = resolved_card_view(&resolved("01A", WireDecision::Deny, None, None));
    assert_eq!(card.outcome, Outcome::Denied);
    assert!(!card.show_pickers);
    assert!(card.attribution.is_none());
}

#[test]
fn an_allowed_card_keeps_the_pickers() {
    let card = resolved_card_view(&resolved("01A", WireDecision::Allow, None, None));
    assert_eq!(card.outcome, Outcome::Allowed);
    assert!(card.show_pickers);
}

#[test]
fn allow_once_reads_as_allowed_rather_than_a_third_badge() {
    // No daemon emits AllowOnce today; if one ever does, the window
    // must not paint an outcome nobody has a mental model for.
    let card = resolved_card_view(&resolved("01A", WireDecision::AllowOnce, None, None));
    assert_eq!(card.outcome, Outcome::Allowed);
}

#[test]
fn only_rule_attributed_allows_get_an_approval_reason() {
    // "See approval reason" is the entry point to Revoke. A
    // human-resolved card has no rule to revoke, so offering the
    // disclosure would dead-end.
    let human = resolved_card_view(&resolved("01A", WireDecision::Allow, None, None));
    assert!(human.attribution.is_none());

    let auto = resolved_card_view(&resolved(
        "01B",
        WireDecision::Allow,
        Some("r-1"),
        Some(Scope::User),
    ));
    let attribution = auto.attribution.expect("auto-allowed card is attributed");
    assert_eq!(attribution.rule_id, "r-1");
    assert!(attribution.summary.contains("r-1"));
    assert!(attribution.summary.contains("user"));
}

#[test]
fn a_denied_card_is_never_attributed_even_if_a_rule_id_leaks_in() {
    // Denies are attributed to denylist rules too. Painting "Revoke"
    // on a card the user was protected from would invite them to
    // delete the rule that did the protecting.
    let card = resolved_card_view(&resolved(
        "01A",
        WireDecision::Deny,
        Some("deny-1"),
        Some(Scope::Denylist),
    ));
    assert!(card.attribution.is_none());
}

#[test]
fn attribution_needs_both_halves() {
    // `rule_scope` is documented as always set alongside `rule_id`.
    // Zipping rather than unwrapping means a future producer that
    // sets only one cannot panic the UI thread.
    let card = resolved_card_view(&resolved("01A", WireDecision::Allow, Some("r-1"), None));
    assert!(card.attribution.is_none());
}

#[test]
fn revoke_is_offered_exactly_for_the_layers_the_window_can_edit() {
    // Session rules live in the same on-disk user file as any other
    // rule, so they are revokable through the identical User remove
    // call — not a dead end. The other three are not user-owned.
    assert_eq!(revoke_scope(Scope::User), Some(WireScope::User));
    assert_eq!(revoke_scope(Scope::Session), Some(WireScope::User));
    assert_eq!(revoke_scope(Scope::Project), None);
    assert_eq!(revoke_scope(Scope::Builtin), None);
    assert_eq!(revoke_scope(Scope::Denylist), None);
}

#[test]
fn a_project_scoped_card_explains_itself_instead_of_offering_revoke() {
    let card = resolved_card_view(&resolved(
        "01A",
        WireDecision::Allow,
        Some("r-1"),
        Some(Scope::Project),
    ));
    let attribution = card.attribution.expect("attributed");
    assert!(
        attribution.revoke.is_none(),
        "project rules are not editable from the window"
    );
    assert!(attribution.summary.contains("project"));
}
