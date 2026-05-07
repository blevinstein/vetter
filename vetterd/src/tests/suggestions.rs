//! Unit tests for [`crate::suggestions`].
//!
//! Integration coverage (admin socket round-trip + auto-approve
//! reaches the worker) lives in `vetterd/tests/suggestions_admin.rs`.
//! These tests exercise the helpers directly against an in-process
//! [`crate::Context`] so failures point at the suggestions module
//! rather than the IPC plumbing.

use std::sync::{Arc, RwLock};

use crate::audit::AuditLog;
use crate::notifier::NoopNotifier;
use crate::pending::PendingQueue;
use crate::testutil::tmpdir;
use crate::Context;
use vetter_core::known_hosts::{self, KnownHostEntry, KnownHostsStore};
use vetter_core::matcher::loader as allow_loader;
use vetter_core::wire::WireScope;

fn build_ctx(allow_path: &std::path::Path) -> Arc<Context> {
    let allowlist = Arc::new(RwLock::new(
        allow_loader::load_default(None, Some(allow_path)).unwrap(),
    ));
    let known_hosts = Arc::new(RwLock::new(known_hosts::load_default(None).unwrap()));
    let audit_path = allow_path.with_file_name("audit.log");
    let audit = Arc::new(AuditLog::open(&audit_path).unwrap());
    let pending = Arc::new(PendingQueue::new());
    let notifier: Arc<dyn crate::notifier::Notifier> = Arc::new(NoopNotifier);
    Arc::new(Context {
        socket_path: allow_path.with_file_name("ignored.sock"),
        audit,
        allowlist,
        known_hosts,
        pending,
        notifier,
        allowlist_override: Some(allow_path.to_path_buf()),
    })
}

#[test]
fn add_allowlist_rule_rejects_project_scope() {
    let dir = tmpdir("vetterd-suggestions-");
    let allow_path = dir.path().join("allowlist.yaml");
    std::fs::write(&allow_path, "rules: []\n").unwrap();
    let ctx = build_ctx(&allow_path);
    let rule = vetter_core::matcher::Rule {
        id: String::new(),
        command: None,
        when: vetter_core::matcher::RuleWhen {
            http: None,
            file_write: None,
            file_read: None,
        },
        note: None,
        created_by: None,
        created_at: None,
    };
    let err = super::add_allowlist_rule(&ctx, WireScope::Project, rule)
        .expect_err("project scope should be rejected in v1");
    assert!(
        matches!(err, super::AddError::UnsupportedScope(WireScope::Project)),
        "got {err:?}"
    );
}

#[test]
fn add_known_host_rejects_project_scope() {
    let dir = tmpdir("vetterd-suggestions-kh-");
    let allow_path = dir.path().join("allowlist.yaml");
    std::fs::write(&allow_path, "rules: []\n").unwrap();
    let ctx = build_ctx(&allow_path);
    let entry = KnownHostEntry {
        pattern: "example.com".into(),
        note: None,
    };
    let err = super::add_known_host(&ctx, WireScope::Project, entry)
        .expect_err("project scope should be rejected in v1");
    assert!(matches!(
        err,
        super::AddError::UnsupportedScope(WireScope::Project)
    ));
}

#[test]
fn suggestions_for_unknown_id_returns_none() {
    let dir = tmpdir("vetterd-suggestions-id-");
    let allow_path = dir.path().join("allowlist.yaml");
    std::fs::write(&allow_path, "rules: []\n").unwrap();
    let ctx = build_ctx(&allow_path);
    assert!(super::suggestions_for(&ctx, "no-such-id").is_none());
}

#[test]
fn remove_allowlist_rule_rejects_project_scope() {
    let dir = tmpdir("vetterd-suggestions-rm-proj-");
    let allow_path = dir.path().join("allowlist.yaml");
    std::fs::write(&allow_path, "rules: []\n").unwrap();
    let ctx = build_ctx(&allow_path);
    let err = super::remove_allowlist_rule(&ctx, WireScope::Project, "any")
        .expect_err("project scope should be rejected in v1");
    assert!(
        matches!(err, super::AddError::UnsupportedScope(WireScope::Project)),
        "got {err:?}"
    );
}

#[test]
fn remove_allowlist_rule_drops_persisted_rule_and_reloads_store() {
    use vetter_core::matcher::rule::{HostPattern, HttpClause, Rule, RuleWhen, UrlClause};
    use vetter_core::HttpMethod;

    let dir = tmpdir("vetterd-suggestions-rm-");
    let allow_path = dir.path().join("allowlist.yaml");
    std::fs::write(&allow_path, "rules: []\n").unwrap();
    let ctx = build_ctx(&allow_path);

    // Persist a rule via the production add path so the YAML round-
    // trip on remove exercises the real loader, not a hand-rolled
    // fixture.
    let rule = Rule {
        id: "trust-api".into(),
        command: None,
        when: RuleWhen {
            http: Some(HttpClause {
                method: Some(vec![HttpMethod::Get]),
                url: Some(UrlClause {
                    scheme: Some("https".into()),
                    host: Some(HostPattern::One("api.test".into())),
                    port: None,
                    path: None,
                }),
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
    };
    let added = super::add_allowlist_rule(&ctx, WireScope::User, rule).expect("add ok");
    assert_eq!(added.id, "trust-api");
    // `load_default(_, override_path)` loads the override file into
    // the `project` layer rather than `user`, mirroring how the
    // matcher layer-resolves a sandboxed config — so we assert
    // against `project` even though the request scope was User.
    // (Same convention `add_allowlist_rule_*` integration tests use.)
    {
        let g = ctx.allowlist.read().unwrap();
        assert!(
            g.project.iter().any(|r| r.id == "trust-api"),
            "rule should be live in the project layer after add: {:?}",
            g
        );
    }

    super::remove_allowlist_rule(&ctx, WireScope::User, "trust-api").expect("remove ok");

    {
        let g = ctx.allowlist.read().unwrap();
        assert!(
            g.project.iter().all(|r| r.id != "trust-api"),
            "rule should be gone from the project layer after remove"
        );
    }
    let body = std::fs::read_to_string(&allow_path).unwrap();
    assert!(
        !body.contains("trust-api"),
        "removed rule must not survive on disk: {body}"
    );
}

#[test]
fn remove_allowlist_rule_unknown_id_propagates_loader_error() {
    let dir = tmpdir("vetterd-suggestions-rm-unknown-");
    let allow_path = dir.path().join("allowlist.yaml");
    std::fs::write(&allow_path, "rules: []\n").unwrap();
    let ctx = build_ctx(&allow_path);
    let err = super::remove_allowlist_rule(&ctx, WireScope::User, "no-such-rule")
        .expect_err("unknown id should not silently succeed");
    // Surface the inner loader error to the operator: the popover
    // alert needs *some* stringifiable failure rather than a silent
    // no-op when a stale popover snapshot races with another revoke.
    assert!(
        matches!(err, super::AddError::Allowlist(_)),
        "expected loader error wrapping, got {err:?}"
    );
}

#[test]
fn known_hosts_default_loads_clean_store_for_test() {
    // Sanity: default known-hosts loads without error in our test
    // sandbox so failures in subsequent tests aren't masked by a
    // panic during ctx construction.
    let store = known_hosts::load_default(None).unwrap();
    let _ = KnownHostsStore { ..store }; // confirms type fields match.
}
