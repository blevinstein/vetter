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
        expires_at: None,
        sid: None,
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
                no_redirects: None,
            }),
            file_write: None,
            file_read: None,
        },
        note: None,
        created_by: None,
        created_at: None,
        expires_at: None,
        sid: None,
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

/// Build a covering `Rule` for `https://api.sid-test.test/v1/**`,
/// optionally scoped to `sid`. Mirrors `covering_rule_for` in
/// `vetterd/tests/suggestions_admin.rs` but lives here since only
/// this file drives `add_allowlist_rule` directly against an
/// in-process `Context` (needed to control `peer_sid` on the
/// synthetic pending summary — the real admin-socket integration
/// tests can't simulate two different caller SIDs from one test
/// process).
fn sid_scoped_rule(sid: Option<i32>) -> vetter_core::matcher::Rule {
    use vetter_core::matcher::rule::{HostPattern, HttpClause, RuleWhen, UrlClause};
    use vetter_core::HttpMethod;
    vetter_core::matcher::Rule {
        id: String::new(),
        command: None,
        when: RuleWhen {
            http: Some(HttpClause {
                method: Some(vec![HttpMethod::Get]),
                url: Some(UrlClause {
                    scheme: Some("https".into()),
                    host: Some(HostPattern::One("api.sid-test.test".into())),
                    port: None,
                    path: Some("/v1/**".into()),
                }),
                headers_allow: Some(vec!["*".into()]),
                no_body: None,
                query: None,
                no_redirects: None,
            }),
            file_write: None,
            file_read: None,
        },
        note: None,
        created_by: None,
        created_at: None,
        expires_at: None,
        sid,
    }
}

fn parsed_get(url: &str) -> vetter_core::ParsedCommand {
    vetter_core::ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into(), url.into()],
        cwd: None,
        effects: vec![vetter_core::Effect::HttpRequest(vetter_core::HttpRequest {
            method: vetter_core::HttpMethod::Get,
            url: url::Url::parse(url).unwrap(),
            headers: vec![],
            body: vetter_core::Body::None,
            auth: None,
            tls: vetter_core::TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        })],
        signals: vec![],
        display_hints: vetter_core::DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

fn pending_summary_with_sid(
    id: &str,
    url: &str,
    peer_sid: Option<i32>,
) -> crate::pending::PromptSummary {
    crate::pending::PromptSummary {
        id: id.into(),
        command: "curl".into(),
        primary_verb: "GET".into(),
        primary_target: url.into(),
        force_prompt: false,
        signals: vec![],
        parsed: Some(parsed_get(url)),
        host_known: vec![false],
        peer_sid,
    }
}

#[test]
fn add_rule_with_matching_sid_auto_approves_only_same_sid_pending() {
    let dir = tmpdir("vetterd-suggestions-sid-");
    let allow_path = dir.path().join("allowlist.yaml");
    std::fs::write(&allow_path, "rules: []\n").unwrap();
    let ctx = build_ctx(&allow_path);

    let url = "https://api.sid-test.test/v1/data";
    let _rx_same = ctx
        .pending
        .submit(pending_summary_with_sid("same-sid", url, Some(42)));
    let _rx_other = ctx
        .pending
        .submit(pending_summary_with_sid("other-sid", url, Some(99)));
    let _rx_none = ctx
        .pending
        .submit(pending_summary_with_sid("no-sid", url, None));

    let added = super::add_allowlist_rule(&ctx, WireScope::User, sid_scoped_rule(Some(42)))
        .expect("add ok");

    assert_eq!(
        added.auto_approved_ids,
        vec!["same-sid".to_string()],
        "only the pending entry sharing the rule's sid should auto-approve"
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
