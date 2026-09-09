//! Tests for [`crate::notifier::linux`]. Layout convention from `AGENTS.md`.
//!
//! Everything here runs without a session bus — CI has none, and a
//! test that needs one would be a test that never runs. The bus
//! plumbing itself is covered by the manual smoke procedure in
//! `plans/LinuxApp.md` §7 steps 5–6; what *is* covered here is every
//! decision the notifier makes before it touches the bus.

use super::*;

use vetter_core::wire::WireDecision;

// ── Capabilities ────────────────────────────────────────────────────────────

#[test]
fn capabilities_read_actions_and_body_markup() {
    let caps = Capabilities::from_list(&["actions", "body-markup", "persistence"]);
    assert!(caps.actions);
    assert!(caps.body_markup);
}

#[test]
fn capabilities_absent_when_server_advertises_neither() {
    // notify-osd's actual capability set: no `actions`, so our
    // buttons would never render (§5.4).
    let caps = Capabilities::from_list(&["body", "icon-static"]);
    assert!(!caps.actions);
    assert!(!caps.body_markup);
}

#[test]
fn capabilities_from_empty_list_is_all_false() {
    let empty: [&str; 0] = [];
    assert_eq!(Capabilities::from_list(&empty), Capabilities::default());
}

#[test]
fn default_capabilities_claim_nothing() {
    // `LinuxNotifier::install` queues the first `GetCapabilities` on
    // the jobs thread instead of blocking startup on it, so there is a
    // brief window where this default *is* the daemon's belief about
    // the server. The window is safe only because the default claims
    // nothing: a banner posted under it degrades to "no buttons, use
    // `vet daemon approve`", which is the documented §5.4 fallback.
    //
    // Flipping any of these to `true` would make that interim state
    // assert support the daemon has never confirmed — banners would
    // advertise Approve/Reject the server may not render, or send
    // unescaped markup. Pinned here because the cost shows up in
    // `install`, a long way from this struct.
    let caps = Capabilities::default();
    assert!(!caps.actions);
    assert!(!caps.body_markup);
}

#[test]
fn capabilities_do_not_match_on_prefix() {
    // `actions-extra` is not `actions`; a sloppy `starts_with` here
    // would claim button support we don't have.
    let caps = Capabilities::from_list(&["actions-extra", "body-markup-ish"]);
    assert!(!caps.actions);
    assert!(!caps.body_markup);
}

// ── Action → decision ───────────────────────────────────────────────────────

#[test]
fn approve_action_maps_to_allow_with_the_promised_audit_reason() {
    let d = decision_for_action("approve").expect("approve is a known action");
    assert_eq!(d.decision, WireDecision::Allow);
    assert_eq!(d.reason, "approved via notification");
}

#[test]
fn reject_action_maps_to_deny_with_the_promised_audit_reason() {
    let d = decision_for_action("reject").expect("reject is a known action");
    assert_eq!(d.decision, WireDecision::Deny);
    assert_eq!(d.reason, "rejected via notification");
}

#[test]
fn default_action_is_not_a_decision() {
    // The spec's `"default"` key is the *body* click. Treating it as
    // an approval would turn "user clicked the banner to read it"
    // into "user authorised the request" — the exact failure mode a
    // security gate cannot have. It becomes the click-through into
    // the approval window in Phase 6d.
    assert!(decision_for_action("default").is_none());
}

#[test]
fn unknown_action_is_not_a_decision() {
    assert!(decision_for_action("").is_none());
    assert!(
        decision_for_action("Approve").is_none(),
        "keys are lowercase"
    );
    assert!(decision_for_action("allow").is_none());
}

// ── Coalescing ──────────────────────────────────────────────────────────────

#[test]
fn banner_is_raised_only_for_the_request_that_found_the_queue_empty() {
    assert!(should_raise_banner(NotifyHint {
        was_empty_before: true
    }));
    assert!(!should_raise_banner(NotifyHint {
        was_empty_before: false
    }));
}

// ── Sound ───────────────────────────────────────────────────────────────────

#[test]
fn sound_hint_follows_the_user_preference() {
    assert_eq!(sound_name_for(true), Some("dialog-question"));
    assert_eq!(sound_name_for(false), None);
}

// ── Body markup escaping ────────────────────────────────────────────────────

#[test]
fn body_markup_escapes_the_xml_entities() {
    assert_eq!(
        escape_body_markup(r#"<b>&"'"#),
        "&lt;b&gt;&amp;&quot;&apos;"
    );
}

#[test]
fn body_markup_leaves_ordinary_text_alone() {
    assert_eq!(
        escape_body_markup("https://example.test/a?b=c"),
        "https://example.test/a?b=c"
    );
}

// ── Banner text ─────────────────────────────────────────────────────────────

fn summary(verb: &str, target: &str) -> PromptSummary {
    PromptSummary {
        id: "01ABCDEFGHIJKLMNOPQRSTUVWX".into(),
        command: "curl".into(),
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
fn banner_text_pairs_verb_and_target() {
    let (title, body) = banner_text(
        &summary("GET", "https://example.test/"),
        Capabilities::default(),
    );
    assert_eq!(title, "vet curl");
    assert_eq!(body, "GET https://example.test/");
}

#[test]
fn banner_text_drops_an_empty_verb() {
    let (_, body) = banner_text(
        &summary("", "https://example.test/"),
        Capabilities::default(),
    );
    assert_eq!(body, "https://example.test/");
}

#[test]
fn banner_text_labels_a_dry_run() {
    // macOS puts this in the subtitle; the freedesktop spec has no
    // subtitle, so it has to ride on the title.
    let mut s = summary("GET", "https://example.test/");
    s.force_prompt = true;
    let (title, _) = banner_text(&s, Capabilities::default());
    assert_eq!(title, "vet curl (dry run)");
}

#[test]
fn banner_text_strips_bidi_overrides_from_the_target() {
    // U+202E RIGHT-TO-LEFT OVERRIDE would otherwise let a hostile URL
    // render reversed, hiding the real host from the human about to
    // click Approve. Same defence the macOS banner applies.
    let s = summary("GET", "https://evil.test/\u{202e}gnp.exe");
    let (_, body) = banner_text(&s, Capabilities::default());
    assert!(
        !body.contains('\u{202e}'),
        "RTLO must not reach the notification server: {body:?}"
    );
}

#[test]
fn banner_text_escapes_markup_only_when_the_server_parses_it() {
    let s = summary("GET", "https://example.test/?q=<b>");
    let plain = banner_text(&s, Capabilities::default()).1;
    let markup = banner_text(
        &s,
        Capabilities {
            actions: false,
            body_markup: true,
        },
    )
    .1;
    assert!(plain.contains("<b>"), "literal server shows it raw");
    assert!(
        markup.contains("&lt;b&gt;") && !markup.contains("<b>"),
        "markup server must get escaped text: {markup:?}"
    );
}

// ── Notification id map ─────────────────────────────────────────────────────

#[test]
fn map_links_both_directions() {
    let mut m = NotificationMap::default();
    m.link(7, "REQ-A");
    assert_eq!(m.request_for(7), Some("REQ-A"));
    assert_eq!(m.forget_request("REQ-A"), Some(7));
    assert_eq!(m.request_for(7), None);
}

#[test]
fn relinking_a_reused_notification_id_evicts_the_stale_request() {
    // Servers reuse ids across restarts (§11). If id 7 comes back for
    // a new request, the old pairing must go — otherwise resolving
    // REQ-A later would close a banner belonging to REQ-B.
    let mut m = NotificationMap::default();
    m.link(7, "REQ-A");
    m.link(7, "REQ-B");
    assert_eq!(m.request_for(7), Some("REQ-B"));
    assert_eq!(m.forget_request("REQ-A"), None, "stale pairing evicted");
    assert_eq!(m.len(), 1);
}

#[test]
fn relinking_a_request_to_a_new_notification_evicts_the_old_id() {
    let mut m = NotificationMap::default();
    m.link(7, "REQ-A");
    m.link(9, "REQ-A");
    assert_eq!(m.request_for(9), Some("REQ-A"));
    assert_eq!(m.request_for(7), None);
    assert_eq!(m.len(), 1);
}

#[test]
fn forgetting_a_notification_drops_both_sides() {
    let mut m = NotificationMap::default();
    m.link(7, "REQ-A");
    assert_eq!(m.forget_notification(7).as_deref(), Some("REQ-A"));
    assert_eq!(m.forget_request("REQ-A"), None);
    assert_eq!(m.len(), 0);
}

#[test]
fn forgetting_an_unknown_id_is_a_no_op() {
    let mut m = NotificationMap::default();
    assert_eq!(m.forget_notification(1), None);
    assert_eq!(m.forget_request("nope"), None);
}

#[test]
fn clear_drops_every_pairing() {
    let mut m = NotificationMap::default();
    m.link(1, "A");
    m.link(2, "B");
    m.clear();
    assert_eq!(m.len(), 0);
    assert_eq!(m.request_for(1), None);
}

// ── Reconciliation ──────────────────────────────────────────────────────────

#[test]
fn banners_close_for_requests_that_left_the_pending_queue() {
    // The whole point of driving this off queue state: "resolved by
    // `vet daemon approve`" and "resolved by clicking Approve" are
    // indistinguishable here, so both close their banner.
    let held = vec!["A".to_string(), "B".to_string(), "C".to_string()];
    let still_pending = vec!["B".to_string()];
    let mut close = banners_to_close(&held, &still_pending);
    close.sort();
    assert_eq!(close, vec!["A".to_string(), "C".to_string()]);
}

#[test]
fn nothing_closes_while_every_request_is_still_pending() {
    let held = vec!["A".to_string(), "B".to_string()];
    assert!(banners_to_close(&held, &held).is_empty());
}

#[test]
fn every_banner_closes_when_the_queue_drains() {
    // What shutdown's `cancel_all` looks like from here.
    let held = vec!["A".to_string(), "B".to_string()];
    assert_eq!(banners_to_close(&held, &[]).len(), 2);
}

// ── Body click-through (Phase 6d step 4) ────────────────────────────────────

#[test]
fn the_two_buttons_still_resolve() {
    // Guards the step-4 refactor: adding a third registered action
    // must not disturb what the existing two mean.
    assert_eq!(
        action_intent("approve"),
        ActionIntent::Resolve(PendingDecision::allow(REASON_APPROVED))
    );
    assert_eq!(
        action_intent("reject"),
        ActionIntent::Resolve(PendingDecision::deny(REASON_REJECTED))
    );
}

#[test]
fn the_body_click_opens_the_window_and_never_resolves() {
    // The safety property of this surface. A banner is easy to brush
    // past, and a body click that approved a command would be the
    // worst failure available here.
    assert_eq!(action_intent("default"), ActionIntent::OpenWindow);
    assert!(
        decision_for_action("default").is_none(),
        "`default` must never map to a decision"
    );
}

#[test]
fn an_unknown_action_key_does_nothing() {
    // Servers may invent keys, and a future action added here would
    // reach daemons that predate it.
    assert_eq!(action_intent("x-kde-something"), ActionIntent::Ignore);
    assert_eq!(
        click_outcome("x-kde-something", Some("R1")),
        ClickOutcome::Nothing
    );
}

#[test]
fn a_button_click_resolves_the_request_the_banner_stands_for() {
    assert_eq!(
        click_outcome("approve", Some("R1")),
        ClickOutcome::Resolve {
            request: "R1".to_string(),
            decision: PendingDecision::allow(REASON_APPROVED),
        }
    );
}

#[test]
fn a_body_click_carries_the_request_id_as_the_scroll_target() {
    assert_eq!(
        click_outcome("default", Some("R1")),
        ClickOutcome::Show {
            card: Some("R1".to_string()),
            picker: None,
        }
    );
}

#[test]
fn a_button_click_on_an_unmapped_banner_resolves_nothing() {
    // The banner outlived our record of it — a server restart, or a
    // banner from a previous daemon run. Guessing at a request would
    // mean deciding somebody's command for them.
    assert_eq!(click_outcome("approve", None), ClickOutcome::Nothing);
    assert_eq!(click_outcome("reject", None), ClickOutcome::Nothing);
}

#[test]
fn a_body_click_still_opens_the_window_with_no_request_to_point_at() {
    // The request resolved between the click and its delivery, so
    // there is no card to scroll to — but the user asked to see
    // Vetter, and the card list is the honest answer.
    assert_eq!(
        click_outcome("default", None),
        ClickOutcome::Show {
            card: None,
            picker: None,
        }
    );
}

#[test]
fn parked_tokens_are_dropped_once_the_map_is_implausibly_large() {
    // Steady state holds at most one: a token is claimed microseconds
    // later by the ActionInvoked it precedes. Growth means a server
    // emitting tokens for interactions that never became actions.
    let mut tokens: HashMap<u32, String> = (0..31).map(|i| (i, format!("t{i}"))).collect();
    prune_tokens(&mut tokens);
    assert_eq!(tokens.len(), 31, "below the cap nothing is dropped");

    tokens.insert(99, "t99".to_string());
    prune_tokens(&mut tokens);
    assert!(
        tokens.is_empty(),
        "at the cap the map is cleared rather than growing without bound"
    );
}

// ── Picker actions (§6i) ────────────────────────────────────────────────────

/// A summary whose host the known-hosts store did not recognise,
/// which is what gates `Trust host…` on both surfaces.
fn unknown_host_summary() -> PromptSummary {
    let mut s = summary("GET", "https://unknown.test/");
    s.signals = vec![vetter_core::RiskSignal {
        kind: vetter_core::SignalKind::UnknownHost,
        detail: "unknown.test".into(),
        effect_idx: Some(0),
    }];
    s
}

#[test]
fn picker_actions_open_a_picker_and_never_resolve() {
    // The whole contract of these two buttons. A banner that said
    // "Allowlist…" and silently approved instead would be a worse
    // betrayal than the stray-body-click case, because the label
    // promised a choice.
    assert_eq!(
        action_intent("allowlist"),
        ActionIntent::OpenPicker(PickerKind::Allowlist)
    );
    assert_eq!(
        action_intent("trust_host"),
        ActionIntent::OpenPicker(PickerKind::TrustHost)
    );
    assert!(decision_for_action("allowlist").is_none());
    assert!(decision_for_action("trust_host").is_none());
}

#[test]
fn a_picker_click_carries_both_the_card_and_which_picker() {
    assert_eq!(
        click_outcome("allowlist", Some("R1")),
        ClickOutcome::Show {
            card: Some("R1".to_string()),
            picker: Some(PickerKind::Allowlist),
        }
    );
    assert_eq!(
        click_outcome("trust_host", Some("R1")),
        ClickOutcome::Show {
            card: Some("R1".to_string()),
            picker: Some(PickerKind::TrustHost),
        }
    );
}

#[test]
fn a_picker_click_on_an_unmapped_banner_opens_the_window_without_the_picker() {
    // A picker generalises one specific request. With that request
    // gone there is nothing to generalise, and a sheet offering tiers
    // for a command the user can no longer see would be asking them
    // to sign something blank.
    assert_eq!(
        click_outcome("allowlist", None),
        ClickOutcome::Show {
            card: None,
            picker: None,
        }
    );
}

// ── Per-request action array ────────────────────────────────────────────────

#[test]
fn no_actions_are_sent_to_a_server_that_cannot_render_them() {
    let caps = Capabilities::from_list(&["body"]);
    assert!(actions_for(&unknown_host_summary(), caps).is_empty());
}

#[test]
fn trust_host_is_offered_only_when_the_host_is_unknown() {
    let caps = Capabilities::from_list(&["actions"]);
    // Same predicate that gates the button on the card, so a banner
    // cannot offer a picker the window would then render empty.
    assert!(actions_for(&unknown_host_summary(), caps).contains(&"trust_host"));
    assert!(!actions_for(&summary("GET", "https://known.test/"), caps).contains(&"trust_host"));
}

#[test]
fn every_banner_offers_the_decisions_and_the_allowlist_shortcut() {
    let caps = Capabilities::from_list(&["actions"]);
    let actions = actions_for(&summary("GET", "https://known.test/"), caps);
    for key in ["default", "approve", "reject", "allowlist"] {
        assert!(actions.contains(&key), "missing `{key}` in {actions:?}");
    }
}

#[test]
fn decisions_are_registered_ahead_of_the_picker_shortcuts() {
    // Positional: a server that truncates a long action list should
    // drop the shortcuts, never Approve or Reject.
    let caps = Capabilities::from_list(&["actions"]);
    let actions = actions_for(&unknown_host_summary(), caps);
    let at = |key: &str| actions.iter().position(|a| *a == key).expect("key present");
    assert!(at("approve") < at("allowlist"));
    assert!(at("reject") < at("allowlist"));
    assert!(at("allowlist") < at("trust_host"));
}

#[test]
fn the_action_array_stays_key_label_paired() {
    // The spec is positional — `[key, label, key, label, …]`. An odd
    // length means some server renders a label as a key.
    let caps = Capabilities::from_list(&["actions"]);
    for summary in [
        unknown_host_summary(),
        summary("GET", "https://known.test/"),
    ] {
        let actions = actions_for(&summary, caps);
        assert_eq!(actions.len() % 2, 0, "unpaired: {actions:?}");
    }
}
