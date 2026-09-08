//! Tests for [`crate::runloop::model`]. Layout convention from
//! `AGENTS.md`.
//!
//! Only the GTK-specific half lives here now. The card view-model,
//! effect rows, resolved-card attribution and picker rows moved to
//! `crate::cards` so both approval surfaces share one definition;
//! their tests moved with them and run on every target.
//!
//! Everything here runs with no display, no session bus and no main
//! loop, since CI has none of the three.

use super::*;

use crate::cards::card::card_view;
use crate::cards::card::snapshot;
use crate::cards::resolved::resolved_snapshot;
use crate::cards::spans::parse_ansi_spans;
use crate::pending::ResolvedEntry;
use crate::testutil::summary;
use vetter_core::wire::WireDecision;

const TEST_PALETTE: MarkupPalette = MarkupPalette {
    red: "#red",
    green: "#green",
    yellow: "#yellow",
    magenta: "#magenta",
    cyan: "#cyan",
    blue: "#blue",
    dim: "#dim",
};

/// Resolved entry with no rule attribution — enough for the
/// live-id pruning tests, which only care about the card's id.
fn resolved(id: &str, decision: WireDecision) -> ResolvedEntry {
    ResolvedEntry {
        summary: summary(id, "curl", "GET", "https://a.example/"),
        rendered: String::new(),
        decision,
        rule_id: None,
        rule_scope: None,
    }
}

// ── Decisions ───────────────────────────────────────────────────────────────

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

// ── Empty state ─────────────────────────────────────────────────────────────
#[test]
fn the_empty_state_points_at_the_other_surfaces() {
    assert!(EMPTY_BODY.contains("vet daemon approve"));
    assert!(EMPTY_BODY.contains("tray"));
    assert!(EMPTY_BODY.contains("notification"));
}

// ── Disclosure state ────────────────────────────────────────────────────────

#[test]
fn an_open_disclosure_survives_a_rebuild() {
    // The regression this exists to stop: the queue changes whenever
    // *any* request resolves anywhere, `refresh` rebuilds every card,
    // and a user reading card A's raw body would have it collapse
    // under them when unrelated card B resolved.
    let mut state = ExpandedState::default();
    state.set_open(Disclosure::Raw, "01A", true);

    state.retain_live(&["01A", "01B"]);

    assert!(
        state.is_open(Disclosure::Raw, "01A"),
        "open state must survive a rebuild"
    );
    assert!(
        !state.is_open(Disclosure::Raw, "01B"),
        "closed is the default"
    );
}

#[test]
fn resolved_ids_are_forgotten_so_the_set_cannot_grow_forever() {
    // Without the retain, every request whose disclosure was ever
    // opened would leave a ULID behind for the daemon's lifetime.
    let mut state = ExpandedState::default();
    state.set_open(Disclosure::Raw, "01A", true);
    state.set_open(Disclosure::Raw, "01B", true);

    state.retain_live(&["01B"]);

    assert!(
        !state.is_open(Disclosure::Raw, "01A"),
        "id that left the window must be dropped"
    );
    assert!(state.is_open(Disclosure::Raw, "01B"));
}

#[test]
fn closing_a_disclosure_clears_it() {
    let mut state = ExpandedState::default();
    state.set_open(Disclosure::Raw, "01A", true);
    state.set_open(Disclosure::Raw, "01A", false);
    assert!(!state.is_open(Disclosure::Raw, "01A"));
}

#[test]
fn live_ids_span_both_sections() {
    // Pruning against pending ids alone would collapse a Recent
    // disclosure the user just opened, because a resolved card is
    // still on screen.
    let pending = snapshot(&[(
        summary("01A", "curl", "GET", "https://a.example/"),
        String::new(),
    )]);
    let recent = resolved_snapshot(&[resolved("01B", WireDecision::Allow)]);
    let ids = live_ids(&pending, &recent);
    assert_eq!(ids, vec!["01A", "01B"]);

    let mut state = ExpandedState::default();
    state.set_open(Disclosure::Reason, "01B", true);
    state.retain_live(&ids);
    assert!(
        state.is_open(Disclosure::Reason, "01B"),
        "a resolved card is still on screen"
    );
}

#[test]
fn the_three_disclosures_on_one_card_are_independent() {
    let mut state = ExpandedState::default();
    state.set_open(Disclosure::Raw, "01A", true);
    assert!(state.is_open(Disclosure::Raw, "01A"));
    assert!(!state.is_open(Disclosure::Details, "01A"));
    assert!(!state.is_open(Disclosure::Reason, "01A"));

    state.set_open(Disclosure::Details, "01A", true);
    state.set_open(Disclosure::Raw, "01A", false);
    assert!(!state.is_open(Disclosure::Raw, "01A"));
    assert!(state.is_open(Disclosure::Details, "01A"));
}

// ── Markup ──────────────────────────────────────────────────────────────────
#[test]
fn markup_escapes_argv_derived_text() {
    // The hole this closes: Pango parses its input as markup, so an
    // unescaped tag in a URL would restyle or hide the text a human
    // reads before approving.
    let card = card_view(
        &summary("01A", "curl", "GET", "x"),
        "curl 'https://evil.test/<span foreground=\"#00ff00\">safe</span>'",
    );
    let markup = spans_to_markup(&card.raw.spans, TEST_PALETTE);
    assert!(
        markup.contains("&lt;span"),
        "the hostile tag must render as text: {markup}"
    );
    assert!(
        !markup.contains("<span foreground=\"#00ff00\""),
        "hostile markup survived into the label: {markup}"
    );
}

#[test]
fn markup_emits_our_own_spans_for_styled_runs() {
    let spans = parse_ansi_spans("\x1b[31mred\x1b[0m plain");
    let markup = spans_to_markup(&spans, TEST_PALETTE);
    assert!(markup.contains("foreground=\"#red\""), "{markup}");
    assert!(
        markup.ends_with(" plain"),
        "unstyled runs stay bare: {markup}"
    );
}

#[test]
fn dim_cyan_is_the_loopback_style_not_the_url_style() {
    // `plans/ApprovalUI.md` maps `2;36` to the muted secondary colour
    // and plain `36` to teal; collapsing the two would paint loopback
    // rows as if they were live remote URLs.
    let loopback = parse_ansi_spans("\x1b[2;36mlocalhost\x1b[0m");
    let url = parse_ansi_spans("\x1b[36mremote\x1b[0m");
    assert!(spans_to_markup(&loopback, TEST_PALETTE).contains("#dim"));
    assert!(spans_to_markup(&url, TEST_PALETTE).contains("#cyan"));
}

#[test]
fn bold_and_underline_survive_into_markup() {
    let spans = parse_ansi_spans("\x1b[1mbold\x1b[0m\x1b[4munder\x1b[0m");
    let markup = spans_to_markup(&spans, TEST_PALETTE);
    assert!(markup.contains("weight=\"bold\""), "{markup}");
    assert!(markup.contains("underline=\"single\""), "{markup}");
}

#[test]
fn an_empty_raw_body_produces_empty_markup() {
    assert_eq!(spans_to_markup(&[], TEST_PALETTE), "");
}
