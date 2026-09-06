//! Tests for [`crate::cards::effects`].
//!
//! These are new. The row-summary logic previously lived inline in
//! `popover_effects`'s AppKit builders, reachable only through a
//! main-thread call, so the wording and the `-d @file` dedup rule
//! had no direct coverage — the existing suite could only assert
//! *row counts*. Now that the strings are platform-neutral they can
//! be pinned directly, on every target.

use std::path::PathBuf;

use super::*;
use vetter_core::{
    Body, DisplayHints, Effect, FormField, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
};

fn parsed(effects: Vec<Effect>) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into()],
        cwd: None,
        effects,
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

fn http(body: Body) -> Effect {
    Effect::HttpRequest(HttpRequest {
        method: HttpMethod::Post,
        url: url::Url::parse("https://example.test/").unwrap(),
        headers: vec![],
        body,
        auth: None,
        tls: TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    })
}

#[test]
fn bodyless_request_gets_no_body_section() {
    // `None` here is what suppresses the whole "body" section, so a
    // bodyless GET doesn't grow a stub label.
    assert_eq!(body_meta_label(&Body::None), None);
}

#[test]
fn body_meta_label_summarises_each_variant() {
    assert_eq!(
        body_meta_label(&Body::Inline {
            bytes: b"1234567".to_vec()
        })
        .as_deref(),
        Some("inline, 7 B")
    );
    assert_eq!(
        body_meta_label(&Body::FromFile {
            path: PathBuf::from("/tmp/x")
        })
        .as_deref(),
        Some("from file")
    );
    assert_eq!(
        body_meta_label(&Body::Form {
            fields: vec![
                FormField {
                    name: "a".into(),
                    value: "1".into()
                },
                FormField {
                    name: "b".into(),
                    value: "2".into()
                },
            ]
        })
        .as_deref(),
        Some("x-www-form-urlencoded, 2 fields")
    );
}

#[test]
fn inline_body_renders_utf8_directly() {
    assert_eq!(inline_body_text(br#"{"x":1}"#), r#"{"x":1}"#);
}

#[test]
fn inline_body_hex_dumps_non_utf8_and_reports_the_remainder() {
    // Invalid UTF-8 falls back to hex. Short input dumps whole.
    assert_eq!(inline_body_text(&[0xff, 0xfe]), "ff fe");

    // Past 64 bytes the dump truncates and says how much was left.
    let long = vec![0xffu8; 70];
    let out = inline_body_text(&long);
    assert!(out.ends_with("… (6 more bytes)"), "{out}");
    assert_eq!(out.matches("ff").count(), 64);
}

#[test]
fn inline_body_sanitises_control_bytes_in_valid_utf8() {
    // Valid UTF-8 carrying an RTLO must not reach a card verbatim.
    let out = inline_body_text("a\u{202e}b".as_bytes());
    assert!(!out.contains('\u{202e}'), "{out:?}");
}

#[test]
fn auth_labels_redact_and_flag_each_variant() {
    use vetter_core::Auth;

    let (label, redacted) = auth_label(&Auth::Basic {
        user: "alice".into(),
        password_redacted: true,
    });
    assert_eq!(label, "Basic user=alice password=••••");
    assert!(redacted);

    let (label, redacted) = auth_label(&Auth::Bearer {
        token_redacted: true,
    });
    assert_eq!(label, "Bearer ••••");
    assert!(redacted);

    let (label, redacted) = auth_label(&Auth::Header {
        name: "X-Api-Key".into(),
    });
    assert_eq!(label, "X-Api-Key: ••••");
    assert!(redacted);

    // `.netrc` credentials never reach us, so there is nothing to
    // redact — and the row renders muted rather than green.
    let (label, redacted) = auth_label(&Auth::Netrc);
    assert_eq!(label, "from .netrc");
    assert!(!redacted);
}

#[test]
fn auth_label_text_does_not_depend_on_the_redaction_flag() {
    use vetter_core::Auth;
    // `Auth::Basic` structurally cannot carry the password — only
    // the username and a flag saying whether a password was
    // redacted upstream. So the *text* must be identical either
    // way, always showing the `••••` recipe; only the colour the
    // caller paints it in varies. A future refactor that started
    // echoing a credential when the flag is false would fail here.
    let (redacted_label, redacted) = auth_label(&Auth::Basic {
        user: "alice".into(),
        password_redacted: true,
    });
    let (plain_label, not_redacted) = auth_label(&Auth::Basic {
        user: "alice".into(),
        password_redacted: false,
    });
    assert_eq!(redacted_label, plain_label);
    assert_eq!(redacted_label, "Basic user=alice password=••••");
    assert!(redacted);
    assert!(!not_redacted);
}

#[test]
fn auth_label_sanitises_the_basic_user() {
    use vetter_core::Auth;
    let (label, _) = auth_label(&Auth::Basic {
        user: "al\u{202e}ice".into(),
        password_redacted: true,
    });
    assert!(!label.contains('\u{202e}'), "{label:?}");
}

#[test]
fn form_field_text_sanitises_name_and_value() {
    let out = form_field_text(&FormField {
        name: "k\u{202e}".into(),
        value: "v\u{0007}".into(),
    });
    assert!(!out.contains('\u{202e}'), "{out:?}");
    assert!(!out.contains('\u{0007}'), "{out:?}");
    assert!(out.contains('='), "{out:?}");
}

#[test]
fn collect_body_file_paths_finds_upload_bodies() {
    let p = parsed(vec![http(Body::FromFile {
        path: PathBuf::from("/tmp/upload.json"),
    })]);
    let set = collect_body_file_paths(&p);
    assert_eq!(set.len(), 1);
    assert!(set.contains(&PathBuf::from("/tmp/upload.json")));
}

#[test]
fn collect_body_file_paths_is_empty_without_a_file_body() {
    let p = parsed(vec![
        http(Body::Inline {
            bytes: b"hi".to_vec(),
        }),
        Effect::FileRead(vetter_core::FileRead {
            path: PathBuf::from("/tmp/in"),
        }),
    ]);
    // A `FileRead` on its own is *not* a body upload — it must not
    // land in the dedup set, or the standalone read row would be
    // suppressed and the path would vanish from the card entirely.
    assert!(collect_body_file_paths(&p).is_empty());
}
