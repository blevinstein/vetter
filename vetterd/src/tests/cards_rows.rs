//! Tests for [`crate::cards::rows`]. Layout convention from `AGENTS.md`.

use super::*;

use crate::testutil::{card_of, http_summary, parsed, request, summary};
use vetter_core::{FormField, Header, HttpMethod};

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

    let rows = card_of(&s).all_rows();
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
            .all_rows()
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

    let rows = card_of(&s).all_rows();
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

    let rows = card_of(&s).all_rows();
    let reads = rows
        .iter()
        .filter(|r| matches!(r, EffectRow::FileRead(_)))
        .count();
    assert_eq!(reads, 0, "the body row already shows that path");
}

#[test]
fn file_inputs_and_metadata_land_in_separate_buckets() {
    // Flattening is one surface's choice, not the model's. A resolved
    // card hides request metadata behind a disclosure while keeping
    // the uploaded file one click away, which is only expressible if
    // the two buckets survive lowering.
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
    let rows = effect_rows(s.parsed.as_ref().expect("parsed set above"));

    assert!(
        matches!(rows.file_inputs.as_slice(), [EffectRow::FileRead(_)]),
        "the read is a file input, got {:?}",
        rows.file_inputs
    );
    assert!(
        matches!(rows.others.as_slice(), [EffectRow::Headers(_)]),
        "headers are metadata, got {:?}",
        rows.others
    );
    assert_eq!(rows.total(), 2);
    assert!(!rows.is_empty());
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

    let rows = card_of(&s).all_rows();
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
    assert!(card_of(&s).all_rows().is_empty());
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
