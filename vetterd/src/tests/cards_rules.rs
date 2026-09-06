//! Tests for [`crate::cards::rules`].
//!
//! New coverage: `popover_picker.rs` shipped without a test module,
//! so the duration ladder and the rule preview had never been tested
//! on any platform. They are exercised here because both are policy
//! the GTK picker must reproduce exactly — a 15-minute rule that
//! quietly persisted forever, or a preview that showed a pattern
//! other than the one written, would be a security regression rather
//! than a cosmetic one.

use super::*;
use vetter_core::matcher::{HostPattern, HttpClause, Rule, RuleWhen, UrlClause};
use vetter_core::HttpMethod;

/// Arbitrary fixed clock. Nothing depends on the value; using a
/// non-zero one keeps `now + delta` from accidentally passing an
/// assertion that a zero clock would.
const NOW: u64 = 1_700_000_000;

/// Session id used for the `peer_sid` plumbing tests.
const SID: i32 = 4242;

/// Every variant, written out longhand rather than reusing
/// [`DurationChoice::ALL`] — these are the tests that check `ALL`
/// itself is complete, so they cannot source their input from it.
/// Adding a variant makes this array a compile error via the
/// exhaustive match in [`variant_index`].
const EVERY_VARIANT: &[DurationChoice] = &[
    DurationChoice::FifteenMinutes,
    DurationChoice::OneHour,
    DurationChoice::FourHours,
    DurationChoice::ThisSession,
    DurationChoice::Forever,
];

/// Exhaustive match over the enum. Exists purely so adding a variant
/// fails to compile here, forcing whoever adds it to decide where it
/// belongs in the ladder instead of silently omitting it from the
/// picker.
fn variant_index(d: DurationChoice) -> usize {
    match d {
        DurationChoice::FifteenMinutes => 0,
        DurationChoice::OneHour => 1,
        DurationChoice::FourHours => 2,
        DurationChoice::ThisSession => 3,
        DurationChoice::Forever => 4,
    }
}

#[test]
fn all_offers_every_variant_exactly_once() {
    assert_eq!(
        DurationChoice::ALL.len(),
        EVERY_VARIANT.len(),
        "a variant was added to DurationChoice but not to ALL, so the \
         picker would never offer it"
    );
    for v in EVERY_VARIANT {
        assert_eq!(
            DurationChoice::ALL.iter().filter(|d| *d == v).count(),
            1,
            "{v:?} should appear in ALL exactly once"
        );
    }
}

#[test]
fn all_is_in_declared_ladder_order() {
    // The picker renders ALL top-to-bottom and looks the default up
    // by position, so the order is load-bearing, not cosmetic.
    let seen: Vec<usize> = DurationChoice::ALL
        .iter()
        .map(|d| variant_index(*d))
        .collect();
    assert_eq!(seen, vec![0, 1, 2, 3, 4]);
}

#[test]
fn forever_is_the_last_entry() {
    // `show_allowlist_picker` selects the default by searching ALL
    // for Forever; keeping it last also matches the "least
    // surprising thing is nearest the button" layout.
    assert_eq!(
        DurationChoice::ALL.last().copied(),
        Some(DurationChoice::Forever)
    );
}

#[test]
fn titles_are_present_and_unambiguous() {
    let mut titles: Vec<&str> = EVERY_VARIANT.iter().map(|d| d.title()).collect();
    assert!(titles.iter().all(|t| !t.trim().is_empty()));
    titles.sort_unstable();
    let before = titles.len();
    titles.dedup();
    assert_eq!(
        titles.len(),
        before,
        "two duration radios would render the same label"
    );
}

#[test]
fn only_this_session_carries_the_peer_sid() {
    // The important half of `apply`: handing a sid to any other
    // choice must not scope the rule to that terminal. A 15-minute
    // rule that silently became session-scoped would stop matching
    // the moment the user opened a new shell.
    for choice in EVERY_VARIANT {
        let (_, sid) = choice.apply(NOW, Some(SID));
        if *choice == DurationChoice::ThisSession {
            assert_eq!(sid, Some(SID), "ThisSession must propagate the sid");
        } else {
            assert_eq!(sid, None, "{choice:?} must discard the caller's sid");
        }
    }
}

#[test]
fn this_session_does_not_invent_a_sid_when_none_was_recorded() {
    // The UI disables this radio when the card never resolved a
    // peer_sid, but the model must not fabricate one if it is
    // reached anyway.
    let (expires, sid) = DurationChoice::ThisSession.apply(NOW, None);
    assert_eq!(sid, None);
    assert!(expires.is_some(), "backstop TTL applies regardless of sid");
}

#[test]
fn forever_sets_neither_expiry_nor_sid() {
    for sid in [None, Some(SID)] {
        assert_eq!(DurationChoice::Forever.apply(NOW, sid), (None, None));
    }
}

#[test]
fn finite_durations_ascend_in_ladder_order() {
    // Pins that the ladder really is ascending — the display order
    // promises "shortest first" and a transposed arm would put a
    // 4-hour rule under the "1 hour" label.
    let expiry = |d: DurationChoice| d.apply(NOW, None).0.expect("finite choice has an expiry");
    let fifteen = expiry(DurationChoice::FifteenMinutes);
    let hour = expiry(DurationChoice::OneHour);
    let four = expiry(DurationChoice::FourHours);
    assert!(NOW < fifteen && fifteen < hour && hour < four);
}

#[test]
fn finite_durations_are_measured_from_the_supplied_clock() {
    // `apply` takes `now` rather than reading the clock itself, so a
    // caller that pre-reads the time gets a consistent pair. Shifting
    // the clock must shift every expiry by the same delta.
    const SHIFT: u64 = 9_999;
    for choice in EVERY_VARIANT {
        let (base, _) = choice.apply(NOW, Some(SID));
        let (shifted, _) = choice.apply(NOW + SHIFT, Some(SID));
        match (base, shifted) {
            (Some(a), Some(b)) => assert_eq!(b - a, SHIFT, "{choice:?} ignored the clock"),
            (None, None) => {} // Forever
            other => panic!("{choice:?} changed shape across clocks: {other:?}"),
        }
    }
}

#[test]
fn session_backstop_outlives_every_explicit_duration() {
    // The backstop exists only so the loader's lazy prune eventually
    // reaps a dead session rule; the sid check is what actually stops
    // it matching. If the backstop ever dropped below an explicit
    // duration it would start expiring live session rules early.
    let backstop = DurationChoice::ThisSession
        .apply(NOW, Some(SID))
        .0
        .expect("session choice has a backstop expiry");
    let longest_explicit = DurationChoice::FourHours
        .apply(NOW, None)
        .0
        .expect("four hours has an expiry");
    assert!(backstop > longest_explicit);
    assert_eq!(backstop - NOW, DurationChoice::SESSION_BACKSTOP_SECS);
}

#[test]
fn confirmation_notes_are_distinct_and_forever_stands_alone() {
    // The confirmation dialog quotes this back after persisting. Two
    // choices sharing a note means the user can be told the wrong
    // lifetime; and any finite choice inheriting the Forever note
    // would claim permanence for a rule that expires.
    let forever_note = DurationChoice::Forever.confirmation_note();
    for choice in EVERY_VARIANT {
        let note = choice.confirmation_note();
        assert!(!note.trim().is_empty(), "{choice:?} has an empty note");
        assert_eq!(
            note == forever_note,
            *choice == DurationChoice::Forever,
            "{choice:?} shares the Forever note but does not persist forever"
        );
    }
    let mut notes: Vec<&str> = EVERY_VARIANT
        .iter()
        .map(|d| d.confirmation_note())
        .collect();
    notes.sort_unstable();
    let before = notes.len();
    notes.dedup();
    assert_eq!(notes.len(), before, "two choices confirm identically");
}

// ── Rule preview ────────────────────────────────────────────────────────────

/// A rule with enough shape to be worth previewing: a method, a
/// scheme, a host, and the noisy autoderived fields the preview is
/// supposed to hide.
fn sample_rule() -> Rule {
    Rule {
        id: "allow-api-test".into(),
        command: Some("curl".into()),
        when: RuleWhen {
            http: Some(HttpClause {
                method: Some(vec![HttpMethod::Get]),
                url: Some(UrlClause {
                    scheme: Some("https".into()),
                    host: Some(HostPattern::One("api.test".into())),
                    port: None,
                    path: None,
                }),
                headers_allow: Some(vec!["*".into()]),
                no_body: None,
                query: None,
                no_redirects: None,
            }),
            file_write: None,
            file_read: None,
        },
        note: Some("added from the popover".into()),
        created_by: Some("vetter".into()),
        created_at: Some("2026-09-06T00:00:00Z".into()),
        expires_at: Some(NOW),
        sid: Some(SID),
    }
}

#[test]
fn preview_shows_the_when_block_and_hides_autoderived_fields() {
    let out = render_rule_when_yaml(&sample_rule());
    assert!(out.starts_with("when:\n"), "preview should lead with when:");
    // The whole point of serialising `rule.when` rather than `rule`.
    for noise in [
        "allow-api-test",
        "id:",
        "created_by:",
        "created_at:",
        "note:",
        "expires_at:",
        "sid:",
    ] {
        assert!(
            !out.contains(noise),
            "preview leaked the autoderived field {noise:?}:\n{out}"
        );
    }
}

#[test]
fn preview_shows_the_host_being_allowed() {
    // The single most important thing the user must see before
    // clicking "Add to user allowlist".
    let out = render_rule_when_yaml(&sample_rule());
    assert!(out.contains("api.test"), "preview omitted the host:\n{out}");
}

#[test]
fn preview_body_adds_a_uniform_two_space_pad_and_keeps_nesting() {
    // The pad is a *prefix*, not a reflow: serde already indents
    // nested mappings, and flattening that would misrepresent the
    // structure of the rule being previewed.
    let rule = sample_rule();
    let raw = serde_yaml_ng::to_string(&rule.when).expect("when serialises");
    let out = render_rule_when_yaml(&rule);

    let body: Vec<&str> = out.lines().skip(1).collect();
    let expected: Vec<&str> = raw.lines().collect();
    assert!(!body.is_empty(), "preview had no body");
    assert_eq!(body.len(), expected.len(), "preview dropped or added lines");
    for (got, want) in body.iter().zip(&expected) {
        assert_eq!(
            *got,
            format!("  {want}"),
            "body line is not the raw line plus a two-space pad"
        );
    }
    // And the nesting really is present, so the assertion above is
    // not vacuously comparing two flat lists.
    assert!(
        expected.iter().any(|l| l.starts_with("  ")),
        "fixture no longer produces nested YAML; the pad test is weakened"
    );
}

#[test]
fn preview_body_is_truthful_yaml_that_parses_back() {
    // The preview must not be a lossy pretty-print: what the user
    // reads has to be what gets written. De-indenting the body and
    // parsing it must reproduce the rule's own `when` clause.
    let rule = sample_rule();
    let out = render_rule_when_yaml(&rule);
    let dedented: String = out
        .lines()
        .skip(1)
        .map(|l| l.strip_prefix("  ").unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n");
    let round_tripped: RuleWhen =
        serde_yaml_ng::from_str(&dedented).expect("preview body should be valid YAML");
    assert_eq!(round_tripped, rule.when);
}

#[test]
fn preview_of_an_empty_when_block_stays_well_formed() {
    // `RuleWhen::default()` skips every field, so serde emits the
    // empty mapping. The preview should still be a single tidy
    // `when:` header plus that mapping, not a ragged string.
    let mut rule = sample_rule();
    rule.when = RuleWhen::default();
    let out = render_rule_when_yaml(&rule);
    assert!(out.starts_with("when:\n"));
    assert!(!out.contains("\n\n"), "blank line in preview:\n{out}");
}

#[test]
fn indent_lines_pads_every_line_without_a_trailing_stub() {
    // serde's output ends with a newline; `lines()` must not turn
    // that into a final pad-only line, which would render as a stray
    // indented blank row under the preview label.
    assert_eq!(indent_lines("a\nb\n", "  "), "  a\n  b");
    assert_eq!(indent_lines("solo", ">>"), ">>solo");
    assert_eq!(indent_lines("", "  "), "");
}
