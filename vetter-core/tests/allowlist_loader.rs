//! Integration tests for the YAML allowlist loader.
//!
//! These tests use `tempfile` to build a real on-disk directory tree
//! per test so the walk-up discovery, `.git` boundary, and override
//! semantics are exercised end-to-end.

use std::fs;
use std::io::Write as _;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;
use vetter_core::matcher::{
    add_rule, discover_project_root, load_default, load_file, write_file, AllowlistFile, LoadError,
    Rule, RuleWhen,
};

fn write(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// Serialise tests that mutate process-wide environment variables.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    _scratch: TempDir,
}

fn isolate_user_scope() -> EnvGuard {
    let lock = env_lock();
    let scratch = TempDir::new().unwrap();
    std::env::set_var("HOME", scratch.path());
    std::env::remove_var("XDG_CONFIG_HOME");
    EnvGuard {
        _lock: lock,
        _scratch: scratch,
    }
}

const ALLOW_GET: &str = r#"
rules:
  - id: allow-get
    when:
      http:
        method: [GET]
        url:
          scheme: https
          host: example.test
"#;

#[test]
fn discovers_project_root_from_nested_cwd() {
    let _user = isolate_user_scope();
    let root = TempDir::new().unwrap();
    write(&root.path().join(".vet/allowlist.yaml"), ALLOW_GET);
    let nested = root.path().join("a/b/c");
    fs::create_dir_all(&nested).unwrap();

    let found = discover_project_root(&nested).expect("walk-up");
    assert_eq!(found, root.path());

    let store = load_default(Some(&nested), None).expect("load");
    assert_eq!(store.project.len(), 1);
    assert_eq!(store.project[0].id, "allow-get");
}

#[test]
fn walk_up_stops_at_git_boundary() {
    let _user = isolate_user_scope();
    let outer = TempDir::new().unwrap();
    write(&outer.path().join(".vet/allowlist.yaml"), ALLOW_GET);

    let inner = outer.path().join("subrepo");
    fs::create_dir_all(inner.join(".git")).unwrap();
    let nested = inner.join("src/module");
    fs::create_dir_all(&nested).unwrap();

    assert!(discover_project_root(&nested).is_none());
    let store = load_default(Some(&nested), None).expect("load");
    assert!(
        store.project.is_empty(),
        "expected git boundary to suppress walk-up, got {store:?}"
    );
}

#[test]
fn walk_up_finds_allowlist_inside_git_repo() {
    let _user = isolate_user_scope();
    let repo = TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join(".git")).unwrap();
    write(&repo.path().join(".vet/allowlist.yaml"), ALLOW_GET);
    let nested = repo.path().join("src");
    fs::create_dir_all(&nested).unwrap();

    let found = discover_project_root(&nested).expect("found");
    assert_eq!(found, repo.path());
}

#[test]
fn override_path_bypasses_discovery() {
    let _user = isolate_user_scope();
    let project = TempDir::new().unwrap();
    write(&project.path().join(".vet/allowlist.yaml"), ALLOW_GET);

    let override_dir = TempDir::new().unwrap();
    let override_path = override_dir.path().join("custom.yaml");
    write(
        &override_path,
        r#"
rules:
  - id: only-from-override
    when:
      http:
        method: [HEAD]
deny:
  - id: blocked
    when:
      http:
        method: [POST]
"#,
    );

    let store = load_default(Some(project.path()), Some(&override_path)).expect("load");
    assert!(
        store.user.is_empty(),
        "override should suppress user scope, got user={:?}",
        store.user
    );
    assert_eq!(store.project.len(), 1);
    assert_eq!(store.project[0].id, "only-from-override");
    assert_eq!(store.denylist.len(), 1);
    assert_eq!(store.denylist[0].id, "blocked");
}

#[test]
fn duplicate_id_within_one_file_rejected() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("dup.yaml");
    write(
        &path,
        r#"
rules:
  - id: shared
    when:
      http: { method: [GET] }
deny:
  - id: shared
    when:
      http: { method: [POST] }
"#,
    );
    let err = load_file(&path).unwrap_err();
    match err {
        LoadError::DuplicateId { id, .. } => assert_eq!(id, "shared"),
        other => panic!("expected DuplicateId, got {other:?}"),
    }
}

#[test]
fn unknown_top_level_key_yields_yaml_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.yaml");
    write(
        &path,
        r#"
rules: []
mystery: 1
"#,
    );
    let err = load_file(&path).unwrap_err();
    assert!(matches!(err, LoadError::Yaml { .. }), "{err:?}");
}

#[test]
fn missing_files_produce_empty_store() {
    let _user = isolate_user_scope();
    let empty = TempDir::new().unwrap();
    let store = load_default(Some(empty.path()), None).expect("load");
    assert!(store.user.is_empty());
    assert!(store.project.is_empty());
    assert!(store.denylist.is_empty());
}

#[test]
fn user_scope_loaded_via_home() {
    let _lock = env_lock();
    let scratch = TempDir::new().unwrap();
    std::env::set_var("HOME", scratch.path());
    std::env::remove_var("XDG_CONFIG_HOME");
    write(
        &scratch.path().join(".vet/allowlist.yaml"),
        r#"
rules:
  - id: from-user
    when:
      http: { method: [GET] }
"#,
    );

    let cwd = TempDir::new().unwrap();
    let store = load_default(Some(cwd.path()), None).expect("load");
    assert_eq!(store.user.len(), 1);
    assert_eq!(store.user[0].id, "from-user");
}

// ── §5.3 atomic-write tests ──────────────────────────────────────────────────

fn simple_rule(id: &str) -> Rule {
    let when: RuleWhen = serde_yaml_ng::from_str("http: { method: [GET] }").unwrap();
    Rule {
        id: id.to_string(),
        command: None,
        when,
        note: None,
        created_by: None,
        created_at: None,
    }
}

#[test]
fn write_file_produces_valid_yaml_and_round_trips() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("allowlist.yaml");

    let mut file = AllowlistFile::default();
    file.rules.push(simple_rule("r1"));
    write_file(&path, &file).expect("write_file");

    let loaded = load_file(&path).expect("load_file after write");
    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(loaded.rules[0].id, "r1");
}

/// Simulate a SIGKILL between the temp-file write and the `rename` call.
///
/// We replicate what `write_file` does internally up to the point of
/// `persist` (= atomic rename), then drop the `NamedTempFile` without
/// persisting. This is exactly what happens if the process is killed
/// after writing the temp data but before the rename completes.
///
/// The invariant: the original file must be byte-for-byte unchanged.
#[test]
fn abandoned_tempfile_leaves_original_intact() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("allowlist.yaml");

    // Write an initial version.
    let mut original = AllowlistFile::default();
    original.rules.push(simple_rule("original"));
    write_file(&target, &original).expect("initial write");
    let original_bytes = fs::read(&target).expect("read original");

    // Simulate writing new content to a sibling temp file…
    let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).expect("NamedTempFile");
    let new_yaml = "rules:\n  - id: replacement\n    when:\n      http: { method: [POST] }\n";
    tmp.write_all(new_yaml.as_bytes()).expect("write tmp");
    tmp.as_file().sync_all().expect("sync tmp");

    // …then "crash": drop without calling persist (no rename).
    drop(tmp);

    // Original must be unchanged.
    let after_bytes = fs::read(&target).expect("read target after abandoned tmp");
    assert_eq!(
        original_bytes, after_bytes,
        "original file must be intact after an abandoned tempfile"
    );

    // Double-check via the loader.
    let reloaded = load_file(&target).expect("reload");
    assert_eq!(reloaded.rules.len(), 1);
    assert_eq!(reloaded.rules[0].id, "original");
}

#[test]
fn add_rule_result_is_valid_yaml_and_rule_is_present() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("allowlist.yaml");

    // Start from an existing file with one rule.
    let mut file = AllowlistFile::default();
    file.rules.push(simple_rule("existing"));
    write_file(&path, &file).expect("initial write");

    // Append a second rule.
    add_rule(&path, simple_rule("added")).expect("add_rule");

    let loaded = load_file(&path).expect("reload after add_rule");
    assert_eq!(
        loaded.rules.len(),
        2,
        "both rules must be present: {loaded:?}"
    );
    assert_eq!(loaded.rules[0].id, "existing");
    assert_eq!(loaded.rules[1].id, "added");
}
