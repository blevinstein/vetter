//! Tests for [`crate::matcher::loader`]. Layout convention is
//! described in `AGENTS.md`.

use super::*;
use crate::matcher::rule::{HttpClause, RuleWhen};
use crate::HttpMethod;
use tempfile::TempDir;

fn rule(id: &str, methods: Vec<HttpMethod>) -> Rule {
    Rule {
        id: id.to_string(),
        command: None,
        when: RuleWhen {
            http: Some(HttpClause {
                method: Some(methods),
                url: None,
                headers_allow: Some(vec!["*".into()]),
                no_body: None,
                query: None,
            }),
            file_write: None,
            file_read: None,
        },
        note: None,
        created_by: None,
        created_at: None,
    }
}

#[test]
fn allowlist_file_round_trip_minimal() {
    let yaml = r#"
rules:
  - id: ok
    when:
      http:
        method: [GET]
"#;
    let f: AllowlistFile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(f.rules.len(), 1);
    assert!(f.deny.is_empty());
}

#[test]
fn allowlist_file_rejects_unknown_top_level_key() {
    let yaml = r#"
rules: []
made_up: 1
"#;
    let err = serde_yaml_ng::from_str::<AllowlistFile>(yaml).unwrap_err();
    assert!(err.to_string().contains("made_up"), "{err}");
}

#[test]
fn write_file_then_load_file_round_trips() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nested/sub/allowlist.yaml");
    let file = AllowlistFile {
        rules: vec![rule("a", vec![HttpMethod::Get])],
        deny: vec![rule("b", vec![HttpMethod::Post])],
    };
    write_file(&path, &file).unwrap();
    let back = load_file(&path).unwrap();
    assert_eq!(back.rules.len(), 1);
    assert_eq!(back.rules[0].id, "a");
    assert_eq!(back.deny.len(), 1);
    assert_eq!(back.deny[0].id, "b");
}

#[test]
fn add_rule_creates_missing_file_with_parents() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a/b/c/allowlist.yaml");
    add_rule(&path, rule("only", vec![HttpMethod::Get])).unwrap();
    assert!(path.exists());
    let back = load_file(&path).unwrap();
    assert_eq!(back.rules.len(), 1);
    assert_eq!(back.rules[0].id, "only");
}

#[test]
fn add_rule_preserves_existing_rules_and_deny() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("allowlist.yaml");
    let initial = AllowlistFile {
        rules: vec![rule("first", vec![HttpMethod::Get])],
        deny: vec![rule("blocked", vec![HttpMethod::Post])],
    };
    write_file(&path, &initial).unwrap();
    add_rule(&path, rule("second", vec![HttpMethod::Head])).unwrap();
    let back = load_file(&path).unwrap();
    assert_eq!(back.rules.len(), 2);
    assert_eq!(back.rules[0].id, "first");
    assert_eq!(back.rules[1].id, "second");
    assert_eq!(back.deny.len(), 1);
    assert_eq!(back.deny[0].id, "blocked");
}

#[test]
fn add_rule_rejects_duplicate_id() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("allowlist.yaml");
    add_rule(&path, rule("dup", vec![HttpMethod::Get])).unwrap();
    let err = add_rule(&path, rule("dup", vec![HttpMethod::Head])).unwrap_err();
    assert!(matches!(err, LoadError::DuplicateId { ref id, .. } if id == "dup"));
}

#[test]
fn remove_rule_from_rules_list() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("allowlist.yaml");
    let initial = AllowlistFile {
        rules: vec![
            rule("keep1", vec![HttpMethod::Get]),
            rule("drop", vec![HttpMethod::Head]),
            rule("keep2", vec![HttpMethod::Get]),
        ],
        deny: vec![rule("blocked", vec![HttpMethod::Post])],
    };
    write_file(&path, &initial).unwrap();
    remove_rule(&path, "drop").unwrap();
    let back = load_file(&path).unwrap();
    let ids: Vec<_> = back.rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["keep1", "keep2"]);
    assert_eq!(back.deny.len(), 1);
    assert_eq!(back.deny[0].id, "blocked");
}

#[test]
fn remove_rule_from_deny_list() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("allowlist.yaml");
    let initial = AllowlistFile {
        rules: vec![rule("keep", vec![HttpMethod::Get])],
        deny: vec![rule("drop", vec![HttpMethod::Post])],
    };
    write_file(&path, &initial).unwrap();
    remove_rule(&path, "drop").unwrap();
    let back = load_file(&path).unwrap();
    assert!(back.deny.is_empty());
    assert_eq!(back.rules.len(), 1);
}

#[test]
fn remove_rule_unknown_id_errors() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("allowlist.yaml");
    write_file(
        &path,
        &AllowlistFile {
            rules: vec![rule("only", vec![HttpMethod::Get])],
            deny: vec![],
        },
    )
    .unwrap();
    let err = remove_rule(&path, "missing").unwrap_err();
    assert!(matches!(err, LoadError::RuleNotFound { ref id, .. } if id == "missing"));
}
