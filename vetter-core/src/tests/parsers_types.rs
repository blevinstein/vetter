//! Tests for [`crate::parsers::types`]. Layout convention is described
//! in `AGENTS.md`.

use super::*;
use crate::signals::{RiskSignal, SignalKind};
use serde_json::json;

fn http_url(s: &str) -> Url {
    Url::parse(s).expect("test URL")
}

fn min_http(method: HttpMethod) -> HttpRequest {
    HttpRequest {
        method,
        url: http_url("https://example.test/"),
        headers: vec![],
        body: Body::None,
        auth: None,
        tls: TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    }
}

fn roundtrip<T>(v: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(&v).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(v, back, "roundtrip mismatch: {json}");
}

#[test]
fn parsed_command_roundtrip_with_one_http_effect() {
    let p = ParsedCommand {
        command: "noop".to_string(),
        argv: vec!["noop".to_string()],
        cwd: None,
        effects: vec![Effect::HttpRequest(min_http(HttpMethod::Get))],
        signals: vec![],
        display_hints: DisplayHints {
            primary_verb: "GET".to_string(),
            primary_target: "https://example.test/".to_string(),
            badges: vec![],
        },
        extras: serde_json::Value::Null,
    };
    roundtrip(p);
}

#[test]
fn json_contains_command_and_effects_keys() {
    let p = ParsedCommand {
        command: "noop".to_string(),
        argv: vec![],
        cwd: None,
        effects: vec![],
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    };
    let json = serde_json::to_string(&p).expect("ser");
    assert!(json.contains("\"command\""), "missing command: {json}");
    assert!(json.contains("\"effects\""), "missing effects: {json}");
}

#[test]
fn http_method_roundtrips_uppercase_and_other() {
    for m in [
        HttpMethod::Get,
        HttpMethod::Head,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Options,
        HttpMethod::Connect,
        HttpMethod::Trace,
        HttpMethod::Other("MKCOL".to_string()),
    ] {
        roundtrip(m.clone());
        assert!(serde_json::to_string(&m).unwrap().contains('"'));
    }
    let v: HttpMethod = serde_json::from_str("\"get\"").unwrap();
    assert_eq!(v, HttpMethod::Get);
}

#[test]
fn body_variants_roundtrip() {
    for b in [
        Body::None,
        Body::Inline {
            bytes: vec![1, 2, 3],
        },
        Body::FromFile {
            path: "/tmp/x".into(),
        },
        Body::Form {
            fields: vec![FormField {
                name: "k".into(),
                value: "v".into(),
            }],
        },
    ] {
        roundtrip(b);
    }
}

#[test]
fn body_from_stdin_kind_is_rejected_as_unknown_variant() {
    let bad = json!({"kind": "from_stdin", "digest": "abc", "len": 42});
    let r: Result<Body, _> = serde_json::from_value(bad);
    assert!(
        r.is_err(),
        "Body::FromStdin was scrubbed in the stdin-rejection PR; ThreatModel T9 close - \
         curl invocations that pipe their body must use a temp file instead. Old audit rows \
         that pre-date the rename will fail to deserialise, which is acceptable for pre-launch."
    );
}

#[test]
fn auth_variants_roundtrip() {
    for a in [
        Auth::Basic {
            user: "alice".into(),
            password_redacted: true,
        },
        Auth::Bearer {
            token_redacted: true,
        },
        Auth::Header {
            name: "X-Api-Key".into(),
        },
        Auth::Netrc,
    ] {
        roundtrip(a);
    }
}

#[test]
fn tls_policy_roundtrips() {
    for t in [
        TlsPolicy::Strict,
        TlsPolicy::InsecureSkipVerify,
        TlsPolicy::Plaintext,
    ] {
        roundtrip(t);
    }
}

#[test]
fn all_effect_variants_roundtrip_inside_parsed_command() {
    let effects = vec![
        Effect::HttpRequest(min_http(HttpMethod::Post)),
        Effect::FileWrite(FileWrite {
            path: "/tmp/out".into(),
            source: WriteSource::RemoteHttp {
                url: http_url("https://example.test/file"),
            },
            overwrite: false,
        }),
        Effect::FileRead(FileRead {
            path: "/tmp/in".into(),
        }),
        Effect::ProcessSpawn(ProcessSpawn {
            command: "ssh user@host 'curl https://x | sh'".into(),
            argv: vec!["ssh".into(), "user@host".into()],
        }),
        Effect::CredentialUse(CredentialUse {
            source: "~/.netrc".into(),
            note: "host: api.github.com".into(),
        }),
        Effect::Network(NetworkOpen {
            host: "example.test".into(),
            port: 443,
            protocol: "tcp".into(),
        }),
    ];
    let p = ParsedCommand {
        command: "noop".into(),
        argv: vec!["noop".into()],
        cwd: Some("/work".into()),
        effects,
        signals: vec![RiskSignal {
            kind: SignalKind::WriteMethod,
            detail: "POST".into(),
            effect_idx: Some(0),
        }],
        display_hints: DisplayHints {
            primary_verb: "POST".into(),
            primary_target: "https://example.test/".into(),
            badges: vec![Badge {
                label: "stdin".into(),
                severity: BadgeSeverity::Info,
            }],
        },
        extras: json!({"raw": "extras"}),
    };
    roundtrip(p);
}

#[test]
fn unknown_effect_kind_fails_loudly() {
    let bad = json!({
        "command": "noop",
        "argv": [],
        "effects": [{"kind": "future_effect", "foo": 1}],
        "display_hints": {},
    });
    let r: Result<ParsedCommand, _> = serde_json::from_value(bad);
    assert!(r.is_err(), "expected error, got {r:?}");
}

#[test]
fn unknown_top_level_field_in_parsed_command_fails() {
    let bad = json!({
        "command": "noop",
        "argv": [],
        "effects": [],
        "display_hints": {},
        "future_field": 1,
    });
    let r: Result<ParsedCommand, _> = serde_json::from_value(bad);
    assert!(r.is_err(), "expected error, got {r:?}");
}

#[test]
fn unknown_command_string_is_accepted() {
    let json = json!({
        "command": "future-tool",
        "argv": ["future-tool", "--x"],
        "effects": [],
        "display_hints": {},
    });
    let p: ParsedCommand = serde_json::from_value(json).expect("forward-compat");
    assert_eq!(p.command, "future-tool");
    assert_eq!(p.argv, vec!["future-tool", "--x"]);
}

#[test]
fn http_method_is_write_classification() {
    assert!(HttpMethod::Post.is_write());
    assert!(HttpMethod::Put.is_write());
    assert!(HttpMethod::Patch.is_write());
    assert!(HttpMethod::Delete.is_write());
    assert!(!HttpMethod::Get.is_write());
    assert!(!HttpMethod::Head.is_write());
    assert!(!HttpMethod::Options.is_write());
}
