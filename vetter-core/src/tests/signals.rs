//! Tests for [`crate::signals`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;
use crate::parsers::{
    Body, DisplayHints, Effect, FileRead, FileWrite, Header, HttpMethod, HttpRequest,
    ParsedCommand, ProcessSpawn, TlsPolicy, WriteSource,
};
use url::Url;

fn pc_with(effects: Vec<Effect>) -> ParsedCommand {
    ParsedCommand {
        command: "noop".into(),
        argv: vec!["noop".into()],
        cwd: Some("/work".into()),
        effects,
        signals: vec![],
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

fn http(method: HttpMethod, url: &str, headers: Vec<(&str, &str)>, tls: TlsPolicy) -> Effect {
    Effect::HttpRequest(HttpRequest {
        method,
        url: Url::parse(url).unwrap(),
        headers: headers
            .into_iter()
            .map(|(n, v)| Header {
                name: n.into(),
                value: v.into(),
            })
            .collect(),
        body: Body::None,
        auth: None,
        tls,
        follow_redirects: false,
        proxy: None,
    })
}

fn kinds(p: &ParsedCommand) -> Vec<SignalKind> {
    analyze(p).into_iter().map(|s| s.kind).collect()
}

#[test]
fn empty_effects_means_empty_signals() {
    let p = pc_with(vec![]);
    assert!(analyze(&p).is_empty());
}

#[test]
fn write_method_triggers_only_for_mutating_methods() {
    for (m, expect) in [
        (HttpMethod::Get, false),
        (HttpMethod::Head, false),
        (HttpMethod::Post, true),
        (HttpMethod::Put, true),
        (HttpMethod::Patch, true),
        (HttpMethod::Delete, true),
        (HttpMethod::Options, false),
    ] {
        let p = pc_with(vec![http(
            m.clone(),
            "https://x.test/",
            vec![],
            TlsPolicy::Strict,
        )]);
        let has = kinds(&p).contains(&SignalKind::WriteMethod);
        assert_eq!(has, expect, "method {:?}", m);
    }
}

#[test]
fn auth_header_triggers_for_known_names_case_insensitive() {
    for h in [
        "Authorization",
        "authorization",
        "Cookie",
        "X-Api-Key",
        "Proxy-Authorization",
        "X-Vault-Token",
    ] {
        let p = pc_with(vec![http(
            HttpMethod::Get,
            "https://x.test/",
            vec![(h, "value")],
            TlsPolicy::Strict,
        )]);
        assert!(
            kinds(&p).contains(&SignalKind::AuthHeader),
            "header {h} should trigger AuthHeader"
        );
    }
    let benign = pc_with(vec![http(
        HttpMethod::Get,
        "https://x.test/",
        vec![("Content-Type", "application/json")],
        TlsPolicy::Strict,
    )]);
    assert!(!kinds(&benign).contains(&SignalKind::AuthHeader));
}

#[test]
fn insecure_tls_via_skipverify() {
    let p = pc_with(vec![http(
        HttpMethod::Get,
        "https://x.test/",
        vec![],
        TlsPolicy::InsecureSkipVerify,
    )]);
    assert!(kinds(&p).contains(&SignalKind::InsecureTls));
}

#[test]
fn plaintext_http_to_non_loopback_triggers_insecure_tls() {
    let p = pc_with(vec![http(
        HttpMethod::Get,
        "http://example.test/",
        vec![],
        TlsPolicy::Plaintext,
    )]);
    assert!(kinds(&p).contains(&SignalKind::InsecureTls));
}

#[test]
fn plaintext_http_to_loopback_does_not_trigger_insecure_tls() {
    for url in [
        "http://localhost/",
        "http://127.0.0.1/",
        "http://127.7.7.7/",
        "http://[::1]/",
    ] {
        let p = pc_with(vec![http(
            HttpMethod::Get,
            url,
            vec![],
            TlsPolicy::Plaintext,
        )]);
        assert!(
            !kinds(&p).contains(&SignalKind::InsecureTls),
            "loopback {url} should not trigger InsecureTls"
        );
    }
}

#[test]
fn non_standard_port_triggers_off_loopback_only() {
    let off = pc_with(vec![http(
        HttpMethod::Get,
        "https://example.test:9999/",
        vec![],
        TlsPolicy::Strict,
    )]);
    assert!(kinds(&off).contains(&SignalKind::NonStandardPort));
    let loopback = pc_with(vec![http(
        HttpMethod::Get,
        "http://localhost:3000/",
        vec![],
        TlsPolicy::Plaintext,
    )]);
    assert!(!kinds(&loopback).contains(&SignalKind::NonStandardPort));
}

#[test]
fn idn_host_triggers_for_punycode_and_unicode() {
    for url in ["https://xn--bcher-kva.example/", "https://пример.test/"] {
        let p = pc_with(vec![http(HttpMethod::Get, url, vec![], TlsPolicy::Strict)]);
        assert!(
            kinds(&p).contains(&SignalKind::IdnHost),
            "url {url} should trigger IdnHost"
        );
    }
}

#[test]
fn raw_ip_literal_triggers_when_not_loopback() {
    let p = pc_with(vec![http(
        HttpMethod::Get,
        "https://1.2.3.4/",
        vec![],
        TlsPolicy::Strict,
    )]);
    assert!(kinds(&p).contains(&SignalKind::RawIpLiteral));
    let loopback = pc_with(vec![http(
        HttpMethod::Get,
        "http://127.0.0.1/",
        vec![],
        TlsPolicy::Plaintext,
    )]);
    assert!(!kinds(&loopback).contains(&SignalKind::RawIpLiteral));
}

#[test]
fn file_outside_cwd_triggers_only_when_outside() {
    let inside = pc_with(vec![Effect::FileWrite(FileWrite {
        path: "/work/output.txt".into(),
        source: WriteSource::Stdin,
        overwrite: false,
    })]);
    assert!(!kinds(&inside).contains(&SignalKind::FileOutsideCwd));
    let outside = pc_with(vec![Effect::FileWrite(FileWrite {
        path: "/etc/passwd".into(),
        source: WriteSource::Stdin,
        overwrite: false,
    })]);
    assert!(kinds(&outside).contains(&SignalKind::FileOutsideCwd));
}

#[test]
fn file_outside_cwd_skipped_when_cwd_unknown() {
    let mut p = pc_with(vec![Effect::FileWrite(FileWrite {
        path: "/etc/passwd".into(),
        source: WriteSource::Stdin,
        overwrite: false,
    })]);
    p.cwd = None;
    assert!(!kinds(&p).contains(&SignalKind::FileOutsideCwd));
}

#[test]
fn file_read_outside_cwd_signal() {
    let p = pc_with(vec![Effect::FileRead(FileRead {
        path: "/etc/shadow".into(),
    })]);
    assert!(kinds(&p).contains(&SignalKind::FileReadOutsideCwd));
}

#[test]
fn pipe_to_shell_triggers_for_known_shells() {
    for cmd in [
        "curl https://x | sh",
        "curl x | bash",
        "curl x | zsh -e",
        "wget -O- x|sh",
    ] {
        let p = pc_with(vec![Effect::ProcessSpawn(ProcessSpawn {
            command: cmd.into(),
            argv: vec![],
        })]);
        assert!(
            kinds(&p).contains(&SignalKind::PipeToShell),
            "cmd `{cmd}` should trigger PipeToShell"
        );
    }
}

#[test]
fn pipe_to_shell_does_not_trigger_for_unrelated_pipes() {
    for cmd in ["curl x | jq .", "ls | sharp", "echo |shell"] {
        let p = pc_with(vec![Effect::ProcessSpawn(ProcessSpawn {
            command: cmd.into(),
            argv: vec![],
        })]);
        assert!(
            !kinds(&p).contains(&SignalKind::PipeToShell),
            "cmd `{cmd}` should not trigger PipeToShell"
        );
    }
}

#[test]
fn analyze_is_pure() {
    let p = pc_with(vec![http(
        HttpMethod::Post,
        "https://example.test:9999/",
        vec![("Authorization", "Bearer x")],
        TlsPolicy::InsecureSkipVerify,
    )]);
    let a = analyze(&p);
    let b = analyze(&p);
    assert_eq!(a, b);
}

#[test]
fn signal_effect_idx_is_valid_or_none() {
    let p = pc_with(vec![http(
        HttpMethod::Post,
        "https://1.2.3.4:9999/",
        vec![("X-Api-Key", "k")],
        TlsPolicy::InsecureSkipVerify,
    )]);
    for s in analyze(&p) {
        if let Some(i) = s.effect_idx {
            assert!(i < p.effects.len());
        }
    }
}

// -- ui_severity classification ----------------------------------

/// Pin the Danger / Warn classification used by the macOS popover
/// (and any future UI). Each kind is asserted explicitly so a future
/// renaming or splitting of `SignalKind` requires re-considering the
/// chip palette deliberately, not silently inheriting the default.
#[test]
fn ui_severity_classifies_each_signal_kind() {
    use crate::parsers::BadgeSeverity::*;
    let cases: &[(SignalKind, crate::parsers::BadgeSeverity)] = &[
        (SignalKind::InsecureFlag, Danger),
        (SignalKind::InsecureTls, Danger),
        (SignalKind::CacertOverride, Danger),
        (SignalKind::ResolveOverride, Danger),
        (SignalKind::UnixSocket, Danger),
        (SignalKind::PipeToShell, Danger),
        (SignalKind::RawIpLiteral, Danger),
        (SignalKind::ClientCertificate, Danger),
        (SignalKind::WriteMethod, Warn),
        (SignalKind::AuthHeader, Warn),
        (SignalKind::NonStandardPort, Warn),
        (SignalKind::IdnHost, Warn),
        (SignalKind::FileOutsideCwd, Warn),
        (SignalKind::FileReadOutsideCwd, Warn),
        (SignalKind::UnknownHost, Warn),
        (SignalKind::RemoteHeaderName, Warn),
        (SignalKind::CreateDirs, Warn),
        (SignalKind::FollowRedirects, Warn),
    ];
    for (kind, expected) in cases {
        assert_eq!(
            kind.ui_severity(),
            *expected,
            "ui_severity for {kind:?} regressed"
        );
    }
}

/// Belt-and-braces: nothing in the current taxonomy is ever rated
/// `Info`. Anything new added later should default to `Info`-tier
/// (no chip) until we explicitly promote it.
#[test]
fn no_current_signal_is_info_tier() {
    use crate::parsers::BadgeSeverity;
    let all = [
        SignalKind::WriteMethod,
        SignalKind::AuthHeader,
        SignalKind::InsecureTls,
        SignalKind::NonStandardPort,
        SignalKind::IdnHost,
        SignalKind::RawIpLiteral,
        SignalKind::FileOutsideCwd,
        SignalKind::FileReadOutsideCwd,
        SignalKind::PipeToShell,
        SignalKind::UnknownHost,
        SignalKind::InsecureFlag,
        SignalKind::ResolveOverride,
        SignalKind::CacertOverride,
        SignalKind::UnixSocket,
        SignalKind::ClientCertificate,
        SignalKind::RemoteHeaderName,
        SignalKind::CreateDirs,
        SignalKind::FollowRedirects,
    ];
    for k in all {
        assert_ne!(
            k.ui_severity(),
            BadgeSeverity::Info,
            "{k:?} unexpectedly classified Info"
        );
    }
}
