//! Tests for [`crate::cards::picker`]. Layout convention from
//! `AGENTS.md`.

use super::*;

use crate::cards::card::card_view;
use crate::testutil::{signal, summary};

// ── Pickers ─────────────────────────────────────────────────────────────────

fn rule_fixture(id: &str) -> Rule {
    Rule {
        id: id.into(),
        command: Some("curl".into()),
        when: Default::default(),
        note: None,
        created_by: None,
        created_at: None,
        expires_at: None,
        sid: None,
    }
}

#[test]
fn trust_host_is_offered_only_when_the_host_is_unknown() {
    // Mirrors the `host_suggestions` engine contract: a button that
    // opens an empty picker is worse than no button.
    assert!(!show_trust_host(&[]));
    assert!(show_trust_host(&[signal(SignalKind::UnknownHost, "")]));
    assert!(!show_trust_host(&[signal(SignalKind::InsecureTls, "")]));
}

#[test]
fn a_card_carries_its_own_trust_host_answer() {
    let mut s = summary("01A", "curl", "GET", "https://a.example/");
    s.signals = vec![signal(SignalKind::UnknownHost, "")];
    assert!(card_view(&s, "").show_trust_host);
    assert!(!card_view(&summary("01B", "curl", "GET", "https://b.example/"), "").show_trust_host);
}

#[test]
fn picker_rows_show_the_tier_and_the_yaml_that_will_be_written() {
    // The preview is the affordance that lets a human see the pattern
    // *before* it is persisted; a picker that wrote a rule the user
    // never read would be a worse surface than one that asks twice.
    let suggestions = vec![RuleSuggestion {
        tier: vetter_core::suggest::SuggestionTier::Exact,
        label: "GET https://a.example/x".into(),
        rule: rule_fixture("r-1"),
    }];
    let rows = rule_picker_rows(&suggestions);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].title.contains("exact"));
    assert!(rows[0].title.contains("GET https://a.example/x"));
    assert!(
        rows[0].preview.starts_with("when:"),
        "preview must be the when-block YAML: {}",
        rows[0].preview
    );
    assert_eq!(
        rows[0].preview,
        crate::cards::rules::render_rule_when_yaml(&suggestions[0].rule),
        "the preview must be exactly what the shared renderer produces"
    );
}

#[test]
fn host_picker_rows_show_the_pattern_being_trusted() {
    let suggestions = vec![HostSuggestion {
        tier: vetter_core::suggest::HostTier::Wildcard,
        label: "*.example".into(),
        entry: vetter_core::known_hosts::KnownHostEntry {
            pattern: "*.example".into(),
            note: None,
        },
    }];
    let rows = host_picker_rows(&suggestions);
    assert!(rows[0].title.contains("wildcard"));
    assert!(
        rows[0].preview.contains("*.example"),
        "the pattern being written must be visible: {}",
        rows[0].preview
    );
}

#[test]
fn a_finite_duration_stamps_an_expiry_and_no_sid() {
    let rule = rule_with_duration(
        rule_fixture("r-1"),
        DurationChoice::FifteenMinutes,
        1_000,
        Some(42),
    );
    assert_eq!(rule.expires_at, Some(1_000 + 15 * 60));
    assert_eq!(
        rule.sid, None,
        "only ThisSession may narrow a rule to a terminal"
    );
}

#[test]
fn this_session_is_the_only_choice_that_carries_the_sid() {
    // A 15-minute rule that quietly became session-scoped would stop
    // matching in a new shell; a session rule that lost its sid would
    // behave like Forever. Both failures are silent, so pin them.
    let session = rule_with_duration(
        rule_fixture("r-1"),
        DurationChoice::ThisSession,
        1_000,
        Some(42),
    );
    assert_eq!(session.sid, Some(42));
    assert!(
        session.expires_at.is_some(),
        "the backstop TTL still applies"
    );

    for choice in [
        DurationChoice::FifteenMinutes,
        DurationChoice::OneHour,
        DurationChoice::FourHours,
        DurationChoice::Forever,
    ] {
        let rule = rule_with_duration(rule_fixture("r-1"), choice, 1_000, Some(42));
        assert_eq!(rule.sid, None, "{choice:?} must not carry a sid");
    }
}

#[test]
fn forever_leaves_the_rule_unbounded() {
    let rule = rule_with_duration(
        rule_fixture("r-1"),
        DurationChoice::Forever,
        1_000,
        Some(42),
    );
    assert_eq!(rule.expires_at, None);
    assert_eq!(rule.sid, None);
}

#[test]
fn stamping_a_duration_preserves_the_rest_of_the_suggested_rule() {
    // The suggestion engine authored `when`; the picker only answers
    // "how long". Overwriting anything else would persist a rule that
    // does not match the preview the user just read.
    let original = rule_fixture("r-1");
    let stamped = rule_with_duration(original.clone(), DurationChoice::OneHour, 1_000, None);
    assert_eq!(stamped.id, original.id);
    assert_eq!(stamped.command, original.command);
    assert_eq!(stamped.when, original.when);
}
