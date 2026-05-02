//! Tests for [`crate::runloop::popover_effects`].
//!
//! Most of the module is AppKit calls that need a real main
//! thread, so the assertions are smoke-only: build a representative
//! `ParsedCommand` and confirm the row count matches what the
//! design doc specifies. Visual styling is verified manually in the
//! Phase 4 smoke runs (and pinned in `plans/ApprovalUI.md`).

#![cfg(target_os = "macos")]

use std::path::PathBuf;

use super::*;
use vetter_core::{
    Auth, Body, Effect, FileRead, FileWrite, FormField, Header, HttpMethod, HttpRequest,
    ParsedCommand, ProcessSpawn, Sha256, TlsPolicy,
};

fn parsed(effects: Vec<Effect>) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into()],
        cwd: None,
        stdin_digest: None,
        effects,
        signals: vec![],
        display_hints: Default::default(),
        extras: serde_json::Value::Null,
    }
}

fn http(headers: Vec<Header>, body: Body, auth: Option<Auth>) -> Effect {
    Effect::HttpRequest(HttpRequest {
        method: HttpMethod::Post,
        url: url::Url::parse("https://example.test/").unwrap(),
        headers,
        body,
        auth,
        tls: TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    })
}

#[test]
fn build_effect_views_emits_no_rows_for_empty_get() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let p = parsed(vec![http(vec![], Body::None, None)]);
    let rows = build_effect_views(&p, mtm);
    assert!(
        rows.is_empty(),
        "GET with no headers / no body / no auth should render no per-effect rows; \
         the URL row at the top of the card already represents the request"
    );
}

#[test]
fn build_effect_views_emits_three_rows_for_full_post() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let p = parsed(vec![http(
        vec![Header {
            name: "Authorization".into(),
            value: "Bearer abcdefgh".into(),
        }],
        Body::Inline {
            bytes: br#"{"x":1}"#.to_vec(),
        },
        Some(Auth::Bearer {
            token_redacted: true,
        }),
    )]);
    let rows = build_effect_views(&p, mtm);
    assert_eq!(
        rows.len(),
        3,
        "a fully-loaded POST should produce headers + body + auth sections"
    );
}

#[test]
fn build_effect_views_emits_one_row_per_file_or_process() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let p = parsed(vec![
        Effect::FileRead(FileRead {
            path: PathBuf::from("/tmp/in"),
        }),
        Effect::FileWrite(FileWrite {
            path: PathBuf::from("/tmp/out"),
            source: vetter_core::WriteSource::Stdin,
            overwrite: false,
        }),
        Effect::ProcessSpawn(ProcessSpawn {
            command: "sh -c 'echo hi'".into(),
            argv: vec!["sh".into(), "-c".into(), "echo hi".into()],
        }),
    ]);
    let rows = build_effect_views(&p, mtm);
    assert_eq!(rows.len(), 3);
}

#[test]
fn build_effect_views_skips_credential_and_network_effects() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let p = parsed(vec![
        Effect::CredentialUse(vetter_core::CredentialUse {
            source: "~/.netrc".into(),
            note: "basic auth".into(),
        }),
        Effect::Network(vetter_core::NetworkOpen {
            host: "example.test".into(),
            port: 443,
            protocol: "tcp".into(),
        }),
    ]);
    let rows = build_effect_views(&p, mtm);
    assert!(
        rows.is_empty(),
        "credential + network effects are skipped in v1"
    );
}

#[test]
fn body_form_renders_one_meta_section_with_field_rows() {
    // Pure smoke test that the Form variant doesn't panic when
    // walked. The visual layout is verified manually.
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let p = parsed(vec![http(
        vec![],
        Body::Form {
            fields: vec![
                FormField {
                    name: "k1".into(),
                    value: "v1".into(),
                },
                FormField {
                    name: "k2".into(),
                    value: "v2".into(),
                },
            ],
        },
        None,
    )]);
    let rows = build_effect_views(&p, mtm);
    // headers omitted, body present, auth omitted → 1 row.
    assert_eq!(rows.len(), 1);
}

#[test]
fn body_from_stdin_renders_meta_only() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let p = parsed(vec![http(
        vec![],
        Body::FromStdin {
            digest: Sha256::new("deadbeef"),
            len: 12,
        },
        None,
    )]);
    let rows = build_effect_views(&p, mtm);
    assert_eq!(rows.len(), 1);
}
