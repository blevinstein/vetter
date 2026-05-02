//! Tests for [`crate::wire`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use crate::{DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy};
use std::io::Cursor;

fn sample_parsed() -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into(), "https://example.test/".into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: url::Url::parse("https://example.test/").unwrap(),
            headers: vec![],
            body: crate::Body::None,
            auth: None,
            tls: TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        })],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

fn sample_request(id: &str) -> VetRequest {
    VetRequest {
        v: PROTOCOL_VERSION,
        id: id.to_string(),
        cwd: Some("/work".into()),
        agent_hint: Some("claude-code".into()),
        command: "curl".into(),
        argv: vec!["curl".into(), "https://example.test/".into()],
        stdin_digest: None,
        parsed: sample_parsed(),
        force_prompt: false,
    }
}

fn sample_decision(id: &str) -> VetDecision {
    VetDecision {
        v: PROTOCOL_VERSION,
        id: id.to_string(),
        decision: WireDecision::Allow,
        reason: "matched rule `r` in user".into(),
        rule_added: None,
    }
}

#[test]
fn request_roundtrips_through_frame() {
    let req = sample_request("01HX0000000000000000000000");
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).unwrap();
    let mut cur = Cursor::new(buf);
    let back: VetRequest = read_frame(&mut cur).unwrap();
    assert_eq!(req, back);
}

#[test]
fn decision_roundtrips_through_frame() {
    let dec = sample_decision("01HX0000000000000000000000");
    let mut buf = Vec::new();
    write_frame(&mut buf, &dec).unwrap();
    let mut cur = Cursor::new(buf);
    let back: VetDecision = read_frame(&mut cur).unwrap();
    assert_eq!(dec, back);
}

#[test]
fn new_request_id_returns_a_ulid_string() {
    let id = new_request_id();
    assert_eq!(id.len(), 26, "ULID should be 26 chars: {id}");
    let parsed = ulid::Ulid::from_string(&id).unwrap();
    assert!(parsed.timestamp_ms() > 0);
}

#[test]
fn version_mismatch_rejected_by_helpers() {
    let mut req = sample_request("01HX0000000000000000000000");
    req.v = 99;
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).unwrap();
    let mut cur = Cursor::new(buf);
    let err = read_request(&mut cur).unwrap_err();
    assert!(matches!(
        err,
        WireError::VersionMismatch { got: 99, want: 1 }
    ));
}

#[test]
fn read_decision_validates_id_match() {
    let dec = sample_decision("AAA");
    let mut buf = Vec::new();
    write_frame(&mut buf, &dec).unwrap();
    let mut cur = Cursor::new(buf);
    let err = read_decision(&mut cur, "BBB").unwrap_err();
    assert!(matches!(err, WireError::IdMismatch { .. }), "{err}");
}

#[test]
fn oversized_frame_rejected_before_alloc() {
    // Hand-build a length prefix announcing 5 MiB; the body should
    // never be read because the cap is checked first.
    let mut buf = Vec::new();
    buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_be_bytes());
    let mut cur = Cursor::new(buf);
    let err: WireError = read_frame::<_, VetRequest>(&mut cur).unwrap_err();
    assert!(matches!(err, WireError::FrameTooLarge { .. }), "{err}");
}

#[test]
fn truncated_body_reported() {
    let req = sample_request("01HX0000000000000000000000");
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).unwrap();
    buf.truncate(buf.len() - 5);
    let mut cur = Cursor::new(buf);
    let err = read_frame::<_, VetRequest>(&mut cur).unwrap_err();
    assert!(matches!(err, WireError::Truncated { .. }), "{err}");
}

#[test]
fn truncated_length_prefix_reported() {
    let mut cur = Cursor::new(vec![0u8, 0u8]);
    let err = read_frame::<_, VetRequest>(&mut cur).unwrap_err();
    assert!(matches!(err, WireError::Truncated { .. }), "{err}");
}

#[test]
fn force_prompt_round_trips_when_set() {
    let mut req = sample_request("01HX0000000000000000000000");
    req.force_prompt = true;
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).unwrap();
    let mut cur = Cursor::new(buf);
    let back: VetRequest = read_frame(&mut cur).unwrap();
    assert!(back.force_prompt);
}

#[test]
fn force_prompt_omitted_from_json_when_false() {
    let req = sample_request("01HX0000000000000000000000");
    let s = serde_json::to_string(&req).unwrap();
    assert!(
        !s.contains("force_prompt"),
        "default false should not appear: {s}"
    );
}

#[test]
fn from_match_maps_decisions_correctly() {
    use crate::matcher::Scope;
    assert_eq!(
        WireDecision::from_match(&MatchDecision::Allow {
            rule_id: "r".into(),
            scope: Scope::User
        }),
        Some(WireDecision::Allow)
    );
    assert_eq!(
        WireDecision::from_match(&MatchDecision::Deny {
            rule_id: "r".into(),
            scope: Scope::Denylist
        }),
        Some(WireDecision::Deny)
    );
    assert_eq!(WireDecision::from_match(&MatchDecision::Prompt), None);
}
