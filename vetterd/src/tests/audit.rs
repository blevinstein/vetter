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
        rule_scope: None,
        force_prompt: false,
        primary_verb: String::new(),
        primary_target: String::new(),
        signals: Vec::new(),
        parsed: None,
        host_known: Vec::new(),
        rendered: String::new(),
        peer_sid: None,
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

// Hardening §H1 / ThreatModel §T8: a freshly created audit log must
// land at mode 0600 (because `argv` is logged verbatim, this is the
// tightest case in the project) and the leaf parent dir we create
// must land at 0700.

#[test]
fn open_creates_file_at_mode_0600() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tmpdir("vetterd-audit-mode-");
    let path = dir.path().join("audit.log");
    let _log = AuditLog::open(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "audit log must land 0600, got 0{mode:o}");
}

#[test]
fn open_creates_parent_dir_at_mode_0700() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tmpdir("vetterd-audit-parent-");
    let parent = dir.path().join("vetter-logs");
    let path = parent.join("audit.log");
    let _log = AuditLog::open(&path).unwrap();
    let mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "newly created parent dir must land 0700, got 0{mode:o}"
    );
}

#[test]
fn open_does_not_silently_chmod_existing_file() {
    // Repair of a pre-existing wide-mode file is the user's job; the
    // doctor surfaces a WARN instead. AuditLog::open must not surprise
    // the user by tightening files it didn't create on this run.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tmpdir("vetterd-audit-noclobber-");
    let path = dir.path().join("audit.log");
    std::fs::write(&path, "").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let _log = AuditLog::open(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o644,
        "open must leave the existing file's mode alone, got 0{mode:o}"
    );
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
        rule_scope: None,
        force_prompt: false,
        primary_verb: "GET".into(),
        primary_target: target.into(),
        signals: Vec::new(),
        parsed: Some(parsed),
        host_known: vec![false],
        rendered: format!("\x1b[1mcurl\x1b[0m {target}"),
        peer_sid: None,
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
fn tail_resolved_entries_returns_newest_first_and_skips_no_card_rows() {
    let dir = tmpdir("vetterd-audit-tail-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();

    // No-card rows: lack `rendered` (e.g. parse failures), should be
    // filtered out by `tail_resolved_entries`.
    log.append(&entry("nocard-1", WireDecision::Allow)).unwrap();
    log.append(&prompt_entry(
        "prompt-a",
        "https://a.test/",
        WireDecision::Allow,
    ))
    .unwrap();
    log.append(&entry("nocard-2", WireDecision::Deny)).unwrap();
    log.append(&prompt_entry(
        "prompt-b",
        "https://b.test/",
        WireDecision::Deny,
    ))
    .unwrap();
    log.append(&entry("nocard-3", WireDecision::Allow)).unwrap();

    let got = log.tail_resolved_entries(10).unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["prompt-b", "prompt-a"],
        "no-card rows must be filtered out and UI rows returned newest-first"
    );
    assert_eq!(got[0].decision, WireDecision::Deny);
    assert_eq!(got[1].decision, WireDecision::Allow);
    // Round-trip check: rich fields survived the tail path.
    assert_eq!(got[0].primary_target, "https://b.test/");
    assert!(got[0].parsed.is_some());
    assert!(!got[0].rendered.is_empty());
}

#[test]
fn tail_resolved_entries_includes_auto_rows_with_attribution() {
    // After Phase 5.1 the daemon writes a `rendered` body for every
    // matcher-attributed auto-decision so the popover Recent ring
    // surfaces it. The tail should pick those rows up alongside
    // human prompt rows.
    let dir = tmpdir("vetterd-audit-tail-auto-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();

    let mut auto_row = prompt_entry("auto-row", "https://api.test/", WireDecision::Allow);
    auto_row.rule_id = Some("trust-api".into());
    auto_row.rule_scope = Some(vetter_core::matcher::Scope::User);
    log.append(&auto_row).unwrap();
    log.append(&prompt_entry(
        "prompt-row",
        "https://a.test/",
        WireDecision::Allow,
    ))
    .unwrap();

    let got = log.tail_resolved_entries(10).unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["prompt-row", "auto-row"]);
    let auto = &got[1];
    assert_eq!(auto.rule_id.as_deref(), Some("trust-api"));
    assert_eq!(auto.rule_scope, Some(vetter_core::matcher::Scope::User));
}

#[test]
fn tail_resolved_entries_honours_cap() {
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
    let got = log.tail_resolved_entries(3).unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["p-6", "p-5", "p-4"]);
}

#[test]
fn tail_resolved_entries_empty_file_is_empty() {
    let dir = tmpdir("vetterd-audit-tail-empty-");
    let path = dir.path().join("audit.log");
    let _log = AuditLog::open(&path).unwrap();
    let log = AuditLog::open(&path).unwrap();
    let got = log.tail_resolved_entries(5).unwrap();
    assert!(got.is_empty());
}

#[test]
fn tail_resolved_entries_handles_torn_trailing_line() {
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
    let got = log.tail_resolved_entries(5).unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["good"]);
}

#[test]
fn tail_resolved_entries_cap_zero_returns_empty() {
    let dir = tmpdir("vetterd-audit-tail-zero-");
    let path = dir.path().join("audit.log");
    let log = AuditLog::open(&path).unwrap();
    log.append(&prompt_entry("p", "https://ok.test/", WireDecision::Allow))
        .unwrap();
    let got = log.tail_resolved_entries(0).unwrap();
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
