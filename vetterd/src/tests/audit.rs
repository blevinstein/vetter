//! Tests for [`crate::audit`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use crate::testutil::tmpdir;
use std::io::Write;
use vetter_core::wire::WireDecision;

fn entry(id: &str, dec: WireDecision) -> AuditEntry {
    AuditEntry {
        id: id.into(),
        timestamp: "epoch:0.0".into(),
        command: "curl".into(),
        argv: vec!["curl".into(), "https://x".into()],
        decision: dec,
        reason: "matched test".into(),
        rule_id: None,
        force_prompt: false,
        primary_verb: String::new(),
        primary_target: String::new(),
        signals: Vec::new(),
        parsed: None,
        host_known: Vec::new(),
        rendered: String::new(),
    }
}

#[test]
fn append_writes_one_json_line_per_entry() {
    let dir = tmpdir("vetterd-audit-test-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();
    log.append(&entry("a", WireDecision::Allow)).unwrap();
    log.append(&entry("b", WireDecision::Deny)).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = body.lines().collect();
    assert_eq!(lines.len(), 2, "{body}");
    let parsed: AuditEntry = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed.id, "a");
    assert_eq!(parsed.decision, WireDecision::Allow);
    let parsed2: AuditEntry = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(parsed2.id, "b");
    assert_eq!(parsed2.decision, WireDecision::Deny);
    assert!(
        body.ends_with('\n'),
        "missing terminating newline: {body:?}"
    );
}

#[test]
fn open_creates_parent_dirs() {
    let dir = tmpdir("vetterd-audit-test-");
    let path = dir.path().join("a/b/c/audit.log");
    let _log = AuditLog::open(&path).unwrap();
    assert!(path.exists());
}

/// Build a prompt-class `AuditEntry` with the richer fields populated
/// so tail / warm-up tests exercise the real prompt path.
fn prompt_entry(id: &str, target: &str, dec: WireDecision) -> AuditEntry {
    use vetter_core::parsers::{
        Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };
    let url = url::Url::parse(target).expect("valid url");
    let parsed = ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into(), target.into()],
        cwd: None,
        stdin_digest: None,
        effects: vec![Effect::HttpRequest(HttpRequest {
            method: HttpMethod::Get,
            url: url.clone(),
            headers: vec![],
            body: Body::None,
            auth: None,
            tls: TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        })],
        signals: vec![],
        display_hints: DisplayHints {
            primary_verb: "GET".into(),
            primary_target: target.into(),
            badges: vec![],
        },
        extras: serde_json::Value::Null,
    };
    AuditEntry {
        id: id.into(),
        timestamp: "epoch:0.0".into(),
        command: "curl".into(),
        argv: vec!["curl".into(), target.into()],
        decision: dec,
        reason: "user responded".into(),
        rule_id: None,
        force_prompt: false,
        primary_verb: "GET".into(),
        primary_target: target.into(),
        signals: Vec::new(),
        parsed: Some(parsed),
        host_known: vec![false],
        rendered: format!("\x1b[1mcurl\x1b[0m {target}"),
    }
}

#[test]
fn serde_round_trip_with_rich_fields() {
    let e = prompt_entry("abc", "https://example.test/", WireDecision::Allow);
    let json = serde_json::to_string(&e).expect("serialise");
    let back: AuditEntry = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, e);
}

#[test]
fn legacy_slim_rows_still_deserialise() {
    let legacy = r#"{
        "id":"x","timestamp":"epoch:0.0","command":"curl",
        "argv":["curl","https://x"],
        "decision":"allow","reason":"matched"
    }"#;
    let entry: AuditEntry = serde_json::from_str(legacy).expect("legacy decode");
    assert_eq!(entry.id, "x");
    assert!(entry.primary_verb.is_empty());
    assert!(entry.primary_target.is_empty());
    assert!(entry.signals.is_empty());
    assert!(entry.parsed.is_none());
    assert!(entry.host_known.is_empty());
    assert!(entry.rendered.is_empty());
}

#[test]
fn slim_rows_do_not_serialise_empty_rich_fields() {
    let e = entry("x", WireDecision::Allow);
    let json = serde_json::to_string(&e).unwrap();
    // Auto-decision rows stay compact: the optional fields are
    // skipped so the audit file doesn't bloat on every request.
    assert!(!json.contains("primary_verb"), "{json}");
    assert!(!json.contains("primary_target"), "{json}");
    assert!(!json.contains("\"signals\""), "{json}");
    assert!(!json.contains("\"parsed\""), "{json}");
    assert!(!json.contains("\"host_known\""), "{json}");
    assert!(!json.contains("\"rendered\""), "{json}");
}

#[test]
fn tail_prompt_entries_returns_newest_first_and_skips_auto_rows() {
    let dir = tmpdir("vetterd-audit-tail-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();

    log.append(&entry("auto-1", WireDecision::Allow)).unwrap();
    log.append(&prompt_entry(
        "prompt-a",
        "https://a.test/",
        WireDecision::Allow,
    ))
    .unwrap();
    log.append(&entry("auto-2", WireDecision::Deny)).unwrap();
    log.append(&prompt_entry(
        "prompt-b",
        "https://b.test/",
        WireDecision::Deny,
    ))
    .unwrap();
    log.append(&entry("auto-3", WireDecision::Allow)).unwrap();

    let got = log.tail_prompt_entries(10).unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["prompt-b", "prompt-a"],
        "auto rows must be filtered out and prompt rows returned newest-first"
    );
    assert_eq!(got[0].decision, WireDecision::Deny);
    assert_eq!(got[1].decision, WireDecision::Allow);
    // Round-trip check: rich fields survived the tail path.
    assert_eq!(got[0].primary_target, "https://b.test/");
    assert!(got[0].parsed.is_some());
    assert!(!got[0].rendered.is_empty());
}

#[test]
fn tail_prompt_entries_honours_cap() {
    let dir = tmpdir("vetterd-audit-tail-cap-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();
    for i in 0..7 {
        log.append(&prompt_entry(
            &format!("p-{i}"),
            "https://example.test/",
            WireDecision::Allow,
        ))
        .unwrap();
    }
    let got = log.tail_prompt_entries(3).unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["p-6", "p-5", "p-4"]);
}

#[test]
fn tail_prompt_entries_empty_file_is_empty() {
    let dir = tmpdir("vetterd-audit-tail-empty-");
    let path = dir.path().join("audit.log");
    let _log = AuditLog::open(&path).unwrap();
    let log = AuditLog::open(&path).unwrap();
    let got = log.tail_prompt_entries(5).unwrap();
    assert!(got.is_empty());
}

#[test]
fn tail_prompt_entries_handles_torn_trailing_line() {
    // A crash mid-write could leave the final line un-terminated and
    // un-parseable. Earlier, complete lines must still come back.
    let dir = tmpdir("vetterd-audit-tail-torn-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();
    log.append(&prompt_entry(
        "good",
        "https://ok.test/",
        WireDecision::Allow,
    ))
    .unwrap();
    drop(log);

    // Append a half-written JSON fragment without a trailing newline.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(b"{\"id\":\"half").unwrap();
    drop(f);

    let log = AuditLog::open(&path).unwrap();
    let got = log.tail_prompt_entries(5).unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["good"]);
}

#[test]
fn tail_prompt_entries_cap_zero_returns_empty() {
    let dir = tmpdir("vetterd-audit-tail-zero-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();
    log.append(&prompt_entry("p", "https://ok.test/", WireDecision::Allow))
        .unwrap();
    let got = log.tail_prompt_entries(0).unwrap();
    assert!(got.is_empty());
}

#[test]
fn append_is_thread_safe() {
    use std::sync::Arc;
    use std::thread;

    let dir = tmpdir("vetterd-audit-test-");
    let path = dir.path().join("audit.log");
    let log = Arc::new(AuditLog::open(&path).unwrap());
    let mut handles = vec![];
    for i in 0..16 {
        let log = Arc::clone(&log);
        handles.push(thread::spawn(move || {
            log.append(&entry(&format!("id-{i}"), WireDecision::Allow))
                .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let body = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = body.lines().collect();
    assert_eq!(lines.len(), 16);
    for line in &lines {
        let _: AuditEntry = serde_json::from_str(line).expect("each line valid JSON");
    }
}
