//! Integration tests for the YAML allowlist loader.
//!
//! These tests use `tempfile` to build a real on-disk directory tree
//! per test so the walk-up discovery, `.git` boundary, and override
//! semantics are exercised end-to-end.

use std::fs;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;
use vetter_core::matcher::{discover_project_root, load_default, load_file, LoadError};

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
