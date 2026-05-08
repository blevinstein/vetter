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

use crate::known_hosts::KnownHostsStore;
use crate::parsers::{
    BadgeSeverity, Effect, FileRead, FileWrite, HttpRequest, ParsedCommand, ProcessSpawn, TlsPolicy,
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
    // --- known-hosts check ---
    /// Host is not present in any layer of the known-hosts list. This signals
    /// that the agent is reaching somewhere the user has not explicitly
    /// recognised as familiar. Not an auto-deny; it requires human review.
    UnknownHost,
    // --- parser-specific (curl, gh, ssh, ...) ---
    InsecureFlag,
    ResolveOverride,
    CacertOverride,
    UnixSocket,
    /// `--cert` / `--key` was supplied to curl, supplying client TLS
    /// material. Treated as Danger because client identity overrides
    /// alter authentication.
    ClientCertificate,
    /// `-J` / `--remote-header-name` was supplied to curl. The on-disk
    /// filename is taken from the response's `Content-Disposition`
    /// header, so the `FileWrite.path` we surface is a placeholder
    /// (URL basename) rather than the path curl will actually use.
    RemoteHeaderName,
    /// `--create-dirs` was supplied to curl. Curl will mkdir-p any
    /// missing parents of the `FileWrite` path, so the write may land
    /// arbitrarily deep below `--output-dir` / `-o`'s parent.
    CreateDirs,
}

impl SignalKind {
    /// UI-tier severity for this signal kind.
    ///
    /// Used by the macOS approver popover (and any future UI) to
    /// decide whether a signal warrants a header chip and which
    /// colour to paint it. The taxonomy is intentionally narrower
    /// than the [`BadgeSeverity`] of the per-effect badge:
    ///
    /// - [`BadgeSeverity::Danger`] for "this should not happen
    ///   without a deliberate decision" (TLS verification
    ///   disabled, custom resolver/CA, raw IP literal, pipe to a
    ///   shell, unix-socket transport).
    /// - [`BadgeSeverity::Warn`] for "worth a glance" (mutating
    ///   HTTP method, auth header present, non-standard port,
    ///   IDN/punycode host, file write/read outside cwd).
    /// - [`BadgeSeverity::Info`] is reserved for future kinds —
    ///   chips are only rendered for `Warn`/`Danger` so anything
    ///   we add later defaults to "stays in the body, no chip".
    ///
    /// Pure function; no I/O.
    pub fn ui_severity(&self) -> BadgeSeverity {
        match self {
            SignalKind::InsecureFlag
            | SignalKind::InsecureTls
            | SignalKind::CacertOverride
            | SignalKind::ResolveOverride
            | SignalKind::UnixSocket
            | SignalKind::PipeToShell
            | SignalKind::RawIpLiteral
            | SignalKind::ClientCertificate => BadgeSeverity::Danger,

            SignalKind::WriteMethod
            | SignalKind::AuthHeader
            | SignalKind::NonStandardPort
            | SignalKind::IdnHost
            | SignalKind::FileOutsideCwd
            | SignalKind::FileReadOutsideCwd
            | SignalKind::UnknownHost
            | SignalKind::RemoteHeaderName
            | SignalKind::CreateDirs => BadgeSeverity::Warn,
        }
    }
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

/// Check each `Effect::HttpRequest` in `p` against the known-hosts store and
/// emit a [`SignalKind::UnknownHost`] for any host that is not recognised.
///
/// Loopback addresses (`localhost`, `127.0.0.1`, `::1`) are always skipped —
/// local dev traffic is never "unknown" in a meaningful sense.
///
/// This is a separate function from [`analyze`] because it requires external
/// data (the store), so it cannot be a pure function over effects alone.
/// Callers should extend `parsed.signals` with its output alongside
/// `analyze(&parsed)`.
pub fn check_known_hosts(p: &ParsedCommand, store: &KnownHostsStore) -> Vec<RiskSignal> {
    let mut out = Vec::new();
    for (idx, eff) in p.effects.iter().enumerate() {
        if let Effect::HttpRequest(req) = eff {
            if is_loopback_host(req.url.host()) {
                continue;
            }
            if let Some(host) = req.url.host_str() {
                if !store.contains(host) {
                    out.push(RiskSignal {
                        kind: SignalKind::UnknownHost,
                        detail: format!("host {host} not in known-hosts list"),
                        effect_idx: Some(idx),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/signals.rs"]
mod tests;
