//! Tests for [`crate::audit`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use crate::testutil::tmpdir;
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
