//! Tests for [`crate::runloop::popover_effects`].
//!
//! Most of the module is AppKit calls that need a real main
//! thread, so the assertions are smoke-only: build a representative
//! `ParsedCommand` and confirm the row count matches what the
//! design doc specifies. Visual styling is verified manually in the
//! Phase 4 smoke runs (and pinned in `plans/ApprovalUI.md`).
//!
//! The `file_button_factory` argument is exercised via a counter
//! closure rather than the real factory: the wiring is what we
//! care about (which effects ask for an Open button, which don't),
//! and the counter pattern needs no `MainThreadMarker` so it runs
//! end-to-end on every CI worker.

#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::path::PathBuf;

use super::*;
use vetter_core::{
    Auth, Body, Effect, FileRead, FileWrite, FormField, Header, HttpMethod, HttpRequest,
    ParsedCommand, ProcessSpawn, Sha256, TlsPolicy,
};

/// No-op factory: returns `None` for every path. Use when a test
/// only cares about row counts or default rendering.
fn no_button(
    _: &std::path::Path,
    _: objc2_foundation::MainThreadMarker,
) -> Option<Retained<NSButton>> {
    None
}

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
    let rows = build_effect_views(&p, mtm, &no_button);
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
    let rows = build_effect_views(&p, mtm, &no_button);
    assert_eq!(
        rows.total(),
        3,
        "a fully-loaded POST should produce headers + body + auth sections"
    );
    assert!(
        rows.file_inputs.is_empty(),
        "Body::Inline is not a file input; the body row belongs in `others`"
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
    let rows = build_effect_views(&p, mtm, &no_button);
    assert_eq!(rows.total(), 3);
    assert_eq!(
        rows.file_inputs.len(),
        1,
        "FileRead is a file input; FileWrite + ProcessSpawn aren't"
    );
    assert_eq!(rows.others.len(), 2);
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
    let rows = build_effect_views(&p, mtm, &no_button);
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
    let rows = build_effect_views(&p, mtm, &no_button);
    // headers omitted, body present, auth omitted → 1 row.
    assert_eq!(rows.total(), 1);
    assert!(
        rows.file_inputs.is_empty(),
        "Body::Form is not a file input; the body row belongs in `others`"
    );
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
    let rows = build_effect_views(&p, mtm, &no_button);
    assert_eq!(rows.total(), 1);
    assert!(
        rows.file_inputs.is_empty(),
        "Body::FromStdin is not a file input; the body row belongs in `others`"
    );
}

// --- Phase 5.1: file-input "Open" button factory wiring ------------

#[test]
fn file_button_factory_called_for_file_read_and_body_from_file() {
    // Threads a counting factory through `build_effect_views` and
    // confirms the factory fires exactly once per *visible* file-
    // input row: the `Body::FromFile` HTTP body. The matching
    // FileRead effect that the curl parser emits alongside `-d
    // @file` is intentionally suppressed on the popover (the body
    // row already shows the path); see the dedup logic in
    // `collect_body_file_paths`.
    //
    // To exercise both the body case and the standalone FileRead
    // case in one test, this fixture uses *different* paths: the
    // body uploads `/tmp/upload.json`, and an unrelated FileRead
    // points at `/tmp/extra.bin`. The factory should fire twice —
    // once per visible row.
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let calls = Cell::new(0_usize);
    let factory = |_: &std::path::Path, _: objc2_foundation::MainThreadMarker| {
        calls.set(calls.get() + 1);
        None::<Retained<NSButton>>
    };
    let p = parsed(vec![
        http(
            vec![],
            Body::FromFile {
                path: PathBuf::from("/tmp/upload.json"),
            },
            None,
        ),
        Effect::FileRead(FileRead {
            path: PathBuf::from("/tmp/extra.bin"),
        }),
    ]);
    let rows = build_effect_views(&p, mtm, &factory);
    assert_eq!(
        calls.get(),
        2,
        "factory should fire once for Body::FromFile and once for the standalone FileRead"
    );
    assert_eq!(
        rows.file_inputs.len(),
        2,
        "both the Body::FromFile body row and the standalone FileRead row are file inputs"
    );
    assert!(
        rows.others.is_empty(),
        "no headers / auth / writes / spawns in this fixture, so `others` stays empty"
    );
}

#[test]
fn file_read_row_dedupes_against_matching_body_from_file() {
    // The curl parser emits both `Body::FromFile { path }` and a
    // sibling `Effect::FileRead { path }` for `-d @file` so the
    // matcher sees the file read independently. The popover must
    // collapse those into a single row, otherwise the user sees
    // the same path twice (one inside the body section, one in a
    // standalone "read" section). This is the regression net.
    //
    // Setup: one HttpRequest with `Body::FromFile` and a FileRead
    // pointing at the *same* path. Expected per-effect rows:
    //   - body section (1 row, with the Open button via factory)
    //   - the duplicate FileRead is suppressed (0 rows)
    // Total: 1 row, and the factory fires once (only for the body
    // path — the suppressed FileRead never reaches the row builder).
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let calls = Cell::new(0_usize);
    let factory = |_: &std::path::Path, _: objc2_foundation::MainThreadMarker| {
        calls.set(calls.get() + 1);
        None::<Retained<NSButton>>
    };
    let dup_path = PathBuf::from("/tmp/upload.json");
    let p = parsed(vec![
        http(
            vec![],
            Body::FromFile {
                path: dup_path.clone(),
            },
            None,
        ),
        Effect::FileRead(FileRead {
            path: dup_path.clone(),
        }),
    ]);
    let rows = build_effect_views(&p, mtm, &factory);
    assert_eq!(
        rows.total(),
        1,
        "FileRead with the same path as Body::FromFile must collapse into the body row"
    );
    assert_eq!(
        rows.file_inputs.len(),
        1,
        "the surviving Body::FromFile row is a file input"
    );
    assert_eq!(
        calls.get(),
        1,
        "factory should fire only for the visible body row, not for the suppressed FileRead"
    );
}

#[test]
fn file_button_factory_not_called_for_file_write_or_process_spawn() {
    // FileWrite paths may not exist yet (Phase 5.1 explicitly
    // skips them this round) and ProcessSpawn isn't a file at all.
    // Both must leave the factory untouched so we don't accidentally
    // start surfacing buttons on outputs / spawns later through a
    // forgotten match arm.
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let calls = Cell::new(0_usize);
    let factory = |_: &std::path::Path, _: objc2_foundation::MainThreadMarker| {
        calls.set(calls.get() + 1);
        None::<Retained<NSButton>>
    };
    let p = parsed(vec![
        Effect::FileWrite(FileWrite {
            path: PathBuf::from("/tmp/out.json"),
            source: vetter_core::WriteSource::Stdin,
            overwrite: false,
        }),
        Effect::ProcessSpawn(ProcessSpawn {
            command: "sh -c 'echo hi'".into(),
            argv: vec!["sh".into(), "-c".into(), "echo hi".into()],
        }),
        http(
            vec![],
            Body::FromStdin {
                digest: Sha256::new("deadbeef"),
                len: 4,
            },
            None,
        ),
    ]);
    let rows = build_effect_views(&p, mtm, &factory);
    assert_eq!(
        calls.get(),
        0,
        "FileWrite, ProcessSpawn, and Body::FromStdin must not invoke the open-file factory"
    );
    assert!(
        rows.file_inputs.is_empty(),
        "none of these effects produce a file-input row"
    );
    assert_eq!(
        rows.others.len(),
        3,
        "FileWrite, ProcessSpawn, and Body::FromStdin all sit in `others`"
    );
}

#[test]
fn file_button_factory_some_keeps_row_count_unchanged() {
    // When the factory returns Some(button) for every call, the
    // row count from `build_effect_views` is identical to the
    // None-returning case — the button is added inside the row,
    // not as its own per-effect row. This pins the per-row
    // semantics so a future "make Open file its own row" change is
    // a deliberate decision, not an accident.
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let p = parsed(vec![
        http(
            vec![],
            Body::FromFile {
                path: PathBuf::from("/tmp/upload.json"),
            },
            None,
        ),
        Effect::FileRead(FileRead {
            path: PathBuf::from("/tmp/upload.json"),
        }),
    ]);
    let none_rows = build_effect_views(&p, mtm, &no_button);
    let some_factory =
        |_: &std::path::Path, mtm: objc2_foundation::MainThreadMarker| Some(NSButton::new(mtm));
    let some_rows = build_effect_views(&p, mtm, &some_factory);
    assert_eq!(none_rows.total(), some_rows.total());
    assert_eq!(none_rows.file_inputs.len(), some_rows.file_inputs.len());
}
