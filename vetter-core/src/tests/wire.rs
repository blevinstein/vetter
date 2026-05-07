//! Tests for [`crate::wire`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use std::io::Cursor;

fn sample_request(id: &str) -> VetRequest {
    VetRequest {
        v: PROTOCOL_VERSION,
        id: id.to_string(),
        cwd: Some("/work".into()),
        agent_hint: Some("claude-code".into()),
        argv: vec!["curl".into(), "https://example.test/".into()],
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
    assert!(
        matches!(
            err,
            WireError::VersionMismatch {
                got: 99,
                want: PROTOCOL_VERSION
            }
        ),
        "{err}"
    );
}

#[test]
fn v1_request_with_legacy_parsed_field_rejected() {
    // A v1 client (or attacker) crafting a request with a `parsed`
    // field tries to bypass the daemon re-parse. With
    // `deny_unknown_fields` on `VetRequest`, the daemon refuses to
    // even decode such a frame, so the lie can't reach the matcher.
    let payload = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "id": "01HX0000000000000000000000",
        "argv": ["curl", "https://evil.test/"],
        "parsed": {
            "command": "curl",
            "argv": ["curl", "https://example.test/"],
            "effects": [],
        },
    });
    let body = serde_json::to_vec(&payload).unwrap();
    let mut buf = Vec::new();
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&body);
    let mut cur = Cursor::new(buf);
    let err = read_request(&mut cur).unwrap_err();
    assert!(matches!(err, WireError::Json(_)), "{err}");
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
fn mgmt_remove_rule_request_round_trips_through_serde() {
    let req = MgmtRequest::RemoveRule {
        scope: WireScope::User,
        id: "trust-api".into(),
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: MgmtRequest = serde_json::from_str(&json).unwrap();
    match back {
        MgmtRequest::RemoveRule { scope, id } => {
            assert_eq!(scope, WireScope::User);
            assert_eq!(id, "trust-api");
        }
        other => panic!("expected RemoveRule, got {other:?}"),
    }
}

#[test]
fn mgmt_rule_removed_response_round_trips_through_serde() {
    let resp = MgmtResponse::RuleRemoved {
        id: "trust-api".into(),
        scope: WireScope::User,
    };
    let json = serde_json::to_string(&resp).unwrap();
    let back: MgmtResponse = serde_json::from_str(&json).unwrap();
    match back {
        MgmtResponse::RuleRemoved { id, scope } => {
            assert_eq!(id, "trust-api");
            assert_eq!(scope, WireScope::User);
        }
        other => panic!("expected RuleRemoved, got {other:?}"),
    }
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
