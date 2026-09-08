//! Tests for [`crate::cards::card`]. Layout convention from `AGENTS.md`.
//!
//! Runs with no display, no session bus and no main loop — which is
//! the point of keeping the lowering separate from widget assembly,
//! since CI has neither.

use super::*;

use crate::cards::url::{HostTrust, MethodTone};
use crate::testutil::{card_of, http_summary, parsed, summary};
use vetter_core::{Effect, HttpMethod, ProcessSpawn};

fn http(card: &CardView) -> &HttpUrlView {
    match &card.url {
        UrlView::Http(h) => h,
        UrlView::Fallback(text) => panic!("expected an HTTP url row, got fallback {text:?}"),
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
    assert_eq!(card.trust, HostTrust::Unknown);
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
    assert_eq!(row.method_tone, MethodTone::Read);
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
        (HttpMethod::Get, MethodTone::Read),
        (HttpMethod::Post, MethodTone::Write),
        (HttpMethod::Delete, MethodTone::Destructive),
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
    assert_eq!(card.trust, HostTrust::Loopback);
    assert_eq!(http(&card).trust, HostTrust::Loopback);
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

// ── Raw body ────────────────────────────────────────────────────────────────
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
