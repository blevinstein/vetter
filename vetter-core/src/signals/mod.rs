//! Generic risk-signal analyzer over [`crate::parsers::ParsedCommand`].
//!
//! Implements the generic items in `plans/Overview.md` §9. Parser-specific
//! signals (curl `--insecure`, etc.) are pushed into
//! [`ParsedCommand::signals`] by the parser itself; this module stays
//! command-agnostic.

use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use url::Host;

use crate::parsers::{
    Effect, FileRead, FileWrite, HttpRequest, ParsedCommand, ProcessSpawn, TlsPolicy,
};

/// One risk observation about a `ParsedCommand`. Signals never auto-deny;
/// they bias the policy layer toward "must look" (Phase 2+).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskSignal {
    pub kind: SignalKind,
    /// Human-readable detail for the renderer / audit log.
    pub detail: String,
    /// Index into `ParsedCommand.effects` if the signal is tied to one
    /// specific effect. `None` for global signals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_idx: Option<usize>,
}

/// Closed enum of recognised signal kinds. Generic kinds are produced by
/// [`analyze`]; parser-specific kinds are produced by parser plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    // --- generic over Effect::HttpRequest ---
    WriteMethod,
    AuthHeader,
    InsecureTls,
    NonStandardPort,
    IdnHost,
    RawIpLiteral,
    // --- generic over FileWrite / FileRead ---
    FileOutsideCwd,
    FileReadOutsideCwd,
    // --- generic over ProcessSpawn ---
    PipeToShell,
    // --- parser-specific (curl, gh, ssh, ...) ---
    InsecureFlag,
    ResolveOverride,
    CacertOverride,
    UnixSocket,
}

const STANDARD_PORTS: &[u16] = &[80, 443, 8080, 8443];
const AUTH_HEADER_NAMES_LC: &[&str] = &[
    "authorization",
    "cookie",
    "x-api-key",
    "proxy-authorization",
];

/// Pure function over `p.effects`. Same input always produces the same
/// output — no I/O, no globals.
pub fn analyze(p: &ParsedCommand) -> Vec<RiskSignal> {
    let mut out = Vec::new();
    for (idx, eff) in p.effects.iter().enumerate() {
        match eff {
            Effect::HttpRequest(req) => analyze_http(req, idx, &mut out),
            Effect::FileWrite(fw) => analyze_file_write(fw, p.cwd.as_deref(), idx, &mut out),
            Effect::FileRead(fr) => analyze_file_read(fr, p.cwd.as_deref(), idx, &mut out),
            Effect::ProcessSpawn(ps) => analyze_process_spawn(ps, idx, &mut out),
            Effect::CredentialUse(_) | Effect::Network(_) => {}
        }
    }
    out
}

fn analyze_http(req: &HttpRequest, idx: usize, out: &mut Vec<RiskSignal>) {
    if req.method.is_write() {
        out.push(RiskSignal {
            kind: SignalKind::WriteMethod,
            detail: format!("method {}", req.method.as_str()),
            effect_idx: Some(idx),
        });
    }

    for h in &req.headers {
        if is_auth_header(&h.name) {
            out.push(RiskSignal {
                kind: SignalKind::AuthHeader,
                detail: format!("{} header present", h.name),
                effect_idx: Some(idx),
            });
        }
    }

    if matches!(req.tls, TlsPolicy::InsecureSkipVerify) {
        out.push(RiskSignal {
            kind: SignalKind::InsecureTls,
            detail: "TLS verification disabled".into(),
            effect_idx: Some(idx),
        });
    } else if matches!(req.tls, TlsPolicy::Plaintext)
        && req.url.scheme() == "http"
        && !is_loopback_host(req.url.host())
    {
        out.push(RiskSignal {
            kind: SignalKind::InsecureTls,
            detail: format!(
                "plaintext http:// to non-loopback host {}",
                req.url.host_str().unwrap_or("?")
            ),
            effect_idx: Some(idx),
        });
    }

    if let Some(port) = req.url.port() {
        if !STANDARD_PORTS.contains(&port) && !is_loopback_host(req.url.host()) {
            out.push(RiskSignal {
                kind: SignalKind::NonStandardPort,
                detail: format!("port {port}"),
                effect_idx: Some(idx),
            });
        }
    }

    if let Some(host) = req.url.host_str() {
        if is_idn_host(host) {
            out.push(RiskSignal {
                kind: SignalKind::IdnHost,
                detail: format!("IDN/punycode host {host}"),
                effect_idx: Some(idx),
            });
        }
        if is_raw_ip_literal(req.url.host()) {
            out.push(RiskSignal {
                kind: SignalKind::RawIpLiteral,
                detail: format!("raw IP host {host}"),
                effect_idx: Some(idx),
            });
        }
    }
}

fn is_auth_header(name: &str) -> bool {
    let lc = name.to_ascii_lowercase();
    if AUTH_HEADER_NAMES_LC.contains(&lc.as_str()) {
        return true;
    }
    if let Some(rest) = lc.strip_prefix("x-") {
        if let Some(stripped) = rest.strip_suffix("-token") {
            // x-<something>-token. Require <something> to be non-empty so
            // bare "x--token" doesn't match accidentally.
            if !stripped.is_empty() {
                return true;
            }
        }
    }
    false
}

fn is_loopback_host(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn is_raw_ip_literal(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Ipv4(ip)) => !ip.is_loopback(),
        Some(Host::Ipv6(ip)) => !ip.is_loopback(),
        Some(Host::Domain(_)) | None => false,
    }
}

fn is_idn_host(host: &str) -> bool {
    if host.contains("xn--") {
        return true;
    }
    if !host.is_ascii() {
        return true;
    }
    // Reject parsed IPs explicitly — `IpAddr::from_str` rejects bracketed
    // IPv6 (e.g. "[::1]") so we strip brackets first to avoid flagging
    // those as IDN.
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if IpAddr::from_str(stripped).is_ok() {
        return false;
    }
    false
}

fn analyze_file_write(fw: &FileWrite, cwd: Option<&Path>, idx: usize, out: &mut Vec<RiskSignal>) {
    if let Some(cwd) = cwd {
        if !path_is_inside(&fw.path, cwd) {
            out.push(RiskSignal {
                kind: SignalKind::FileOutsideCwd,
                detail: format!("{} is outside cwd", fw.path.display()),
                effect_idx: Some(idx),
            });
        }
    }
}

fn analyze_file_read(fr: &FileRead, cwd: Option<&Path>, idx: usize, out: &mut Vec<RiskSignal>) {
    if let Some(cwd) = cwd {
        if !path_is_inside(&fr.path, cwd) {
            out.push(RiskSignal {
                kind: SignalKind::FileReadOutsideCwd,
                detail: format!("{} is outside cwd", fr.path.display()),
                effect_idx: Some(idx),
            });
        }
    }
}

/// True if `path` is inside `cwd`. Both must be absolute already (the
/// parser is responsible for normalisation per `Overview.md` §8.2).
/// Performs a logical comparison; does not touch the filesystem.
fn path_is_inside(path: &Path, cwd: &Path) -> bool {
    let p = normalise(path);
    let c = normalise(cwd);
    p.starts_with(&c)
}

fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn analyze_process_spawn(ps: &ProcessSpawn, idx: usize, out: &mut Vec<RiskSignal>) {
    if pipes_to_shell(&ps.command) {
        out.push(RiskSignal {
            kind: SignalKind::PipeToShell,
            detail: format!("pipe to shell in `{}`", ps.command),
            effect_idx: Some(idx),
        });
    }
}

/// Hand-rolled scan for `| sh`, `| bash`, `| zsh` boundaries. We don't
/// pull a regex dep just for this.
fn pipes_to_shell(cmd: &str) -> bool {
    let s = cmd.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'|' {
            let mut j = i + 1;
            while j < s.len() && (s[j] == b' ' || s[j] == b'\t') {
                j += 1;
            }
            const SHELLS: &[&[u8]] = &[b"sh", b"bash", b"zsh"];
            for shell in SHELLS {
                let end = j + shell.len();
                if end <= s.len()
                    && &s[j..end] == *shell
                    && (end == s.len() || !is_word_byte(s[end]))
                {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[cfg(test)]
mod tests {
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
            stdin_digest: None,
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
}
