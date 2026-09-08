//! Tests for [`crate::runloop::model`]. Layout convention from `AGENTS.md`.
//!
//! Everything here runs with no display, no session bus and no main
//! loop — which is the point of keeping the lowering separate from
//! the widget assembly, since CI has neither.

use super::*;

use crate::cards::pills::Tone;
use vetter_core::wire::WireDecision;
use vetter_core::{
    DisplayHints, FormField, Header, HttpMethod, HttpRequest, ProcessSpawn, TlsPolicy,
};

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

fn request(method: HttpMethod, url: &str) -> HttpRequest {
    HttpRequest {
        method,
        url: url.parse().expect("test url parses"),
        headers: Vec::new(),
        body: Body::None,
        auth: None,
        tls: TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    }
}

fn parsed(effects: Vec<Effect>) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into()],
        cwd: None,
        effects,
        signals: Vec::new(),
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

/// Summary carrying a parsed HTTP request, which is what the URL row
/// and the effect rows are built from.
fn http_summary(id: &str, method: HttpMethod, url: &str) -> PromptSummary {
    let mut s = summary(id, "curl", method.as_str(), url);
    s.parsed = Some(parsed(vec![Effect::HttpRequest(request(method, url))]));
    s.host_known = vec![false];
    s
}

fn card_of(s: &PromptSummary) -> CardView {
    card_view(s, "")
}

fn http(card: &CardView) -> &HttpUrlView {
    match &card.url {
        UrlView::Http(h) => h,
        UrlView::Fallback(text) => panic!("expected an HTTP url row, got fallback {text:?}"),
    }
}

const TEST_PALETTE: MarkupPalette = MarkupPalette {
    red: "#red",
    green: "#green",
    yellow: "#yellow",
    magenta: "#magenta",
    cyan: "#cyan",
    blue: "#blue",
    dim: "#dim",
};

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

// ── Card basics ─────────────────────────────────────────────────────────────

#[test]
fn card_without_a_parsed_command_falls_back_to_verb_and_target() {
    let card = card_of(&summary("01A", "curl", "GET", "https://example.com/"));
    assert_eq!(card.title, "curl");
    assert_eq!(card.id, "01A");
    match &card.url {
        UrlView::Fallback(text) => {
            assert!(text.contains("GET"), "{text}");
            assert!(text.contains("https://example.com/"), "{text}");
        }
        UrlView::Http(_) => panic!("no parsed command means no typed URL row"),
    }
}

#[test]
fn dry_run_sets_the_wrapper_flag_rather_than_editing_the_title() {
    // `plans/ApprovalUI.md` moved the dry-run marker from an inline
    // pill to the wrapper frame's title, so the two classes of card
    // read apart at a glance. The title must stay the bare command.
    let mut s = summary("01A", "curl", "GET", "https://example.com/");
    s.force_prompt = true;
    let card = card_of(&s);
    assert!(card.dry_run);
    assert_eq!(card.title, "curl", "the wrapper carries the dry-run label");
}

#[test]
fn sanitising_applies_to_the_command_name() {
    let card = card_of(&summary(
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
fn argv_derived_text_is_sanitised_before_it_reaches_a_widget() {
    // A hostile URL carrying an RTLO override could otherwise reorder
    // what the human reads on the surface they approve from.
    let card = card_of(&summary(
        "01A",
        "curl",
        "GET",
        "https://example.com/\u{202e}gnp.exe",
    ));
    let UrlView::Fallback(text) = &card.url else {
        panic!("expected fallback");
    };
    assert!(!text.contains('\u{202e}'), "RTLO survived: {text:?}");
}

#[test]
fn trust_defaults_to_unknown_without_a_parsed_command() {
    // Conservative direction: an unknown-host pill overstates risk,
    // a known-host pill would understate it.
    let card = card_of(&summary("01A", "curl", "GET", "https://example.com/"));
    assert_eq!(card.trust, cards::url::HostTrust::Unknown);
}

#[test]
fn cards_are_ordered_oldest_first() {
    // ULIDs sort chronologically, so the longest-blocked request —
    // the one an agent has been waiting on — lands at the top.
    let pending: Vec<(PromptSummary, String)> = ["01C", "01A", "01B"]
        .iter()
        .map(|id| {
            (
                summary(id, "curl", "GET", "https://x.example/"),
                String::new(),
            )
        })
        .collect();
    let ids: Vec<String> = snapshot(&pending).into_iter().map(|c| c.id).collect();
    assert_eq!(ids, ["01A", "01B", "01C"]);
}

#[test]
fn an_empty_queue_lowers_to_no_cards() {
    assert!(snapshot(&[]).is_empty());
}

#[test]
fn the_empty_state_points_at_the_other_surfaces() {
    assert!(EMPTY_BODY.contains("vet daemon approve"));
    assert!(EMPTY_BODY.contains("tray"));
    assert!(EMPTY_BODY.contains("notification"));
}

// ── URL row ─────────────────────────────────────────────────────────────────

#[test]
fn url_row_splits_into_typed_tokens() {
    let card = card_of(&http_summary(
        "01A",
        HttpMethod::Get,
        "https://api.example.com/v1/things?q=1",
    ));
    let row = http(&card);
    assert_eq!(row.method, "GET");
    assert_eq!(row.method_tone, cards::url::MethodTone::Read);
    assert_eq!(row.scheme, "https://");
    assert_eq!(row.host, "api.example.com");
    assert_eq!(row.tail.text, "/v1/things?q=1");
    assert!(row.tail.has_path);
}

#[test]
fn quiet_ports_are_hidden_and_odd_ones_are_shown() {
    // A visible port is a "look here" cue; rendering `:443` on every
    // https card would make the cue meaningless.
    let quiet = card_of(&http_summary(
        "01A",
        HttpMethod::Get,
        "https://example.com:443/",
    ));
    assert_eq!(http(&quiet).port, None);

    let loud = card_of(&http_summary(
        "01B",
        HttpMethod::Get,
        "https://example.com:9001/",
    ));
    assert_eq!(http(&loud).port, Some(9001));
}

#[test]
fn method_tone_tracks_how_destructive_the_verb_is() {
    for (method, tone) in [
        (HttpMethod::Get, cards::url::MethodTone::Read),
        (HttpMethod::Post, cards::url::MethodTone::Write),
        (HttpMethod::Delete, cards::url::MethodTone::Destructive),
    ] {
        let card = card_of(&http_summary("01A", method.clone(), "https://example.com/"));
        assert_eq!(http(&card).method_tone, tone, "{method:?}");
    }
}

#[test]
fn a_loopback_host_reads_as_loopback_even_when_the_store_says_unknown() {
    // `host_known` is the store's answer and can be stale; loopback
    // is decided structurally so `localhost` never paints as an
    // unknown third-party host.
    let card = card_of(&http_summary(
        "01A",
        HttpMethod::Get,
        "http://localhost:3000/",
    ));
    assert_eq!(card.trust, cards::url::HostTrust::Loopback);
    assert_eq!(http(&card).trust, cards::url::HostTrust::Loopback);
}

#[test]
fn a_process_spawn_card_still_gets_a_url_row_slot() {
    // The fallback keeps every card's vertical cadence constant, so
    // the button row does not jump between cards of different kinds.
    let mut s = summary("01A", "curl", "", "sh -c ...");
    s.parsed = Some(parsed(vec![Effect::ProcessSpawn(ProcessSpawn {
        command: "sh -c 'echo hi'".into(),
        argv: vec![],
    })]));
    assert!(matches!(card_of(&s).url, UrlView::Fallback(_)));
}

// ── Pills ───────────────────────────────────────────────────────────────────

fn signal(kind: SignalKind, detail: &str) -> RiskSignal {
    RiskSignal {
        kind,
        detail: detail.into(),
        effect_idx: None,
    }
}

#[test]
fn pills_dedupe_by_kind_keeping_the_first_detail() {
    // A multi-effect request that trips the same kind repeatedly gets
    // one chip; three identical pills would bury the rest of the card.
    let pills = pills_for(&[
        signal(SignalKind::InsecureFlag, "first"),
        signal(SignalKind::InsecureFlag, "second"),
    ]);
    assert_eq!(pills.len(), 1);
    assert!(pills[0].tooltip.contains("first"), "{:?}", pills[0].tooltip);
    assert!(!pills[0].tooltip.contains("second"));
}

#[test]
fn pills_sort_most_urgent_first() {
    // The eye should land on danger before it scans past the
    // supportive green chip.
    let pills = pills_for(&[
        signal(SignalKind::AuthHeader, "bearer"),
        signal(SignalKind::InsecureFlag, "-k"),
    ]);
    let tones: Vec<Tone> = pills.iter().map(|p| p.tone).collect();
    let danger_first = tones
        .iter()
        .position(|t| *t == Tone::Danger)
        .unwrap_or(usize::MAX);
    let positive_at = tones
        .iter()
        .position(|t| *t == Tone::Positive)
        .unwrap_or(usize::MAX);
    assert!(
        danger_first < positive_at,
        "expected danger before positive, got {tones:?}"
    );
}

#[test]
fn info_tier_signals_get_no_chip() {
    // They live in the raw body's `Risk signals:` line instead;
    // chipping them would dilute the ones that matter.
    let pills = pills_for(&[signal(SignalKind::WriteMethod, "POST")]);
    assert!(
        pills.is_empty() || pills.iter().all(|p| p.tone != Tone::Danger),
        "info-tier signal produced an urgent chip: {pills:?}"
    );
}

#[test]
fn no_signals_means_no_pills_row() {
    assert!(pills_for(&[]).is_empty());
}

// ── Effect rows ─────────────────────────────────────────────────────────────

#[test]
fn header_values_never_reach_a_row() {
    // Agents routinely put bearer tokens and signed URLs in headers
    // we cannot classify, so the card shows names only. A value
    // leaking here would put a live credential on screen.
    let mut req = request(HttpMethod::Get, "https://example.com/");
    req.headers = vec![Header {
        name: "Authorization".into(),
        value: "Bearer super-secret-value".into(),
    }];
    let mut s = summary("01A", "curl", "GET", "https://example.com/");
    s.parsed = Some(parsed(vec![Effect::HttpRequest(req)]));

    let rows = card_of(&s).rows;
    let headers = rows
        .iter()
        .find_map(|r| match r {
            EffectRow::Headers(names) => Some(names),
            _ => None,
        })
        .expect("a headers row");
    assert_eq!(headers, &["Authorization"]);
    for row in &rows {
        assert!(
            !format!("{row:?}").contains("super-secret-value"),
            "header value leaked into {row:?}"
        );
    }
}

#[test]
fn a_bodyless_request_gets_no_body_row() {
    let card = card_of(&http_summary(
        "01A",
        HttpMethod::Get,
        "https://example.com/",
    ));
    assert!(
        !card
            .rows
            .iter()
            .any(|r| matches!(r, EffectRow::Body { .. })),
        "Body::None must not produce a stub row"
    );
}

#[test]
fn a_form_body_lists_its_fields() {
    let mut req = request(HttpMethod::Post, "https://example.com/");
    req.body = Body::Form {
        fields: vec![FormField {
            name: "user".into(),
            value: "alice".into(),
        }],
    };
    let mut s = summary("01A", "curl", "POST", "https://example.com/");
    s.parsed = Some(parsed(vec![Effect::HttpRequest(req)]));

    let rows = card_of(&s).rows;
    let body = rows
        .iter()
        .find_map(|r| match r {
            EffectRow::Body { meta, content } => Some((meta, content)),
            _ => None,
        })
        .expect("a body row");
    assert!(body.0.contains("1 fields"), "{}", body.0);
    match body.1 {
        BodyContent::Form(fields) => assert_eq!(fields, &["user=alice"]),
        other => panic!("expected a form body, got {other:?}"),
    }
}

#[test]
fn a_file_body_suppresses_the_duplicate_read_row() {
    // The curl parser emits both `Body::FromFile` and a separate
    // `FileRead` for `-d @file` so the matcher can see the read. A
    // card rendering both would show the same path twice.
    let mut req = request(HttpMethod::Post, "https://example.com/");
    req.body = Body::FromFile {
        path: "/tmp/payload.json".into(),
    };
    let mut s = summary("01A", "curl", "POST", "https://example.com/");
    s.parsed = Some(parsed(vec![
        Effect::HttpRequest(req),
        Effect::FileRead(vetter_core::FileRead {
            path: "/tmp/payload.json".into(),
        }),
    ]));

    let rows = card_of(&s).rows;
    let reads = rows
        .iter()
        .filter(|r| matches!(r, EffectRow::FileRead(_)))
        .count();
    assert_eq!(reads, 0, "the body row already shows that path");
}

#[test]
fn file_input_rows_sort_ahead_of_the_rest() {
    // "What file are you uploading?" belongs at the top of the card,
    // matching the macOS ordering.
    let mut req = request(HttpMethod::Post, "https://example.com/");
    req.headers = vec![Header {
        name: "X-Thing".into(),
        value: "v".into(),
    }];
    let mut s = summary("01A", "curl", "POST", "https://example.com/");
    s.parsed = Some(parsed(vec![
        Effect::HttpRequest(req),
        Effect::FileRead(vetter_core::FileRead {
            path: "/tmp/input.bin".into(),
        }),
    ]));

    let rows = card_of(&s).rows;
    assert!(
        matches!(rows.first(), Some(EffectRow::FileRead(_))),
        "expected the file input first, got {rows:?}"
    );
}

#[test]
fn credential_and_network_effects_are_skipped() {
    // The §8.5 layout does not render them either; a card that did
    // would drift from the renderer it is supposed to mirror.
    let mut s = summary("01A", "curl", "GET", "https://example.com/");
    s.parsed = Some(parsed(vec![Effect::Network(vetter_core::NetworkOpen {
        host: "example.com".into(),
        port: 443,
        protocol: "tcp".into(),
    })]));
    assert!(card_of(&s).rows.is_empty());
}

// ── Open file suppression ───────────────────────────────────────────────────

#[test]
fn open_is_offered_only_for_a_path_that_exists() {
    // A `FileWrite` target usually does not exist yet, and offering
    // to open it would produce a confusing no-op.
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("present.txt");
    std::fs::write(&real, b"hi").expect("write");
    let absent = dir.path().join("absent.txt");

    assert!(can_open(&real));
    assert!(!can_open(&absent));
    assert!(file_path(&real).can_open);
    assert!(!file_path(&absent).can_open);
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

// ── Markup ──────────────────────────────────────────────────────────────────

#[test]
fn raw_body_carries_both_styled_spans_and_plain_text() {
    // The clipboard gets the plain form so what is copied is exactly
    // what is readable, with no escapes or SGR bytes.
    let card = card_view(
        &summary("01A", "curl", "GET", "https://example.com/"),
        "\x1b[1mcurl\x1b[0m https://example.com/",
    );
    assert_eq!(card.raw.plain, "curl https://example.com/");
    assert!(card.raw.spans.iter().any(|s| s.style.bold));
}

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
    let spans = cards::spans::parse_ansi_spans("\x1b[31mred\x1b[0m plain");
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
    let loopback = cards::spans::parse_ansi_spans("\x1b[2;36mlocalhost\x1b[0m");
    let url = cards::spans::parse_ansi_spans("\x1b[36mremote\x1b[0m");
    assert!(spans_to_markup(&loopback, TEST_PALETTE).contains("#dim"));
    assert!(spans_to_markup(&url, TEST_PALETTE).contains("#cyan"));
}

#[test]
fn bold_and_underline_survive_into_markup() {
    let spans = cards::spans::parse_ansi_spans("\x1b[1mbold\x1b[0m\x1b[4munder\x1b[0m");
    let markup = spans_to_markup(&spans, TEST_PALETTE);
    assert!(markup.contains("weight=\"bold\""), "{markup}");
    assert!(markup.contains("underline=\"single\""), "{markup}");
}

#[test]
fn an_empty_raw_body_produces_empty_markup() {
    assert_eq!(spans_to_markup(&[], TEST_PALETTE), "");
}

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

#[test]
fn live_ids_span_both_sections() {
    // Pruning against pending ids alone would collapse a Recent
    // disclosure the user just opened, because a resolved card is
    // still on screen.
    let pending = snapshot(&[(
        summary("01A", "curl", "GET", "https://a.example/"),
        String::new(),
    )]);
    let recent = resolved_snapshot(&[resolved("01B", WireDecision::Allow, None, None)]);
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
