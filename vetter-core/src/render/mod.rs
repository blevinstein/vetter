//! Generic renderer that walks `ParsedCommand.effects` to produce the
//! styled summary block from `plans/Overview.md` §8.5.
//!
//! The renderer is command-agnostic: it never branches on
//! `ParsedCommand.command`. ANSI colouring is the writer's
//! responsibility — the renderer emits styled chunks via [`StyledWriter`]
//! and the binary chooses [`PlainWriter`] (tests, redirected stderr) or
//! [`AnsiWriter`] (TTY).
//!
//! ## Trust boundary (H2 / `plans/ThreatModel.md`)
//!
//! Every chunk derived from `ParsedCommand` (URLs, header names +
//! values, paths, body content, auth labels, badges, display hints,
//! rule ids) MUST flow through [`escape::sanitize_for_display`]
//! before reaching `write_styled` / `plain`. Renderer-owned literals
//! (the `─` rule line, indents, `Match:` label, scope/severity text,
//! signal slugs from [`signal_kind_label`]) bypass it — they're the
//! only legitimate source of control bytes / non-printable codepoints
//! in the rendered output, and the writer's own ANSI SGR escapes are
//! emitted *outside* `write_styled`'s `s` argument by [`AnsiWriter`].

use std::io::{self, Write};

use crate::matcher::{Decision, Scope};
use crate::parsers::{
    Auth, Badge, BadgeSeverity, Body, Effect, FileRead, FileWrite, Header, HttpMethod, HttpRequest,
    ParsedCommand, ProcessSpawn,
};
use crate::signals::{RiskSignal, SignalKind};

mod escape;
mod redact;

pub use escape::sanitize_for_display;
pub use redact::redact_value;

/// Style tag for a chunk of output. The writer implementation decides
/// whether to translate it into ANSI escape codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Header,
    RuleLine,
    Method(HttpMethodColour),
    /// Header name (e.g. `Authorization:`).
    HeaderName,
    /// Header value that has been redacted before reaching us.
    /// Header rows always use this style — the renderer treats every
    /// header value as sensitive (see [`redact`] module docs).
    RedactedHeader,
    /// Plain-text auth label (today: only the `Auth: via ~/.netrc`
    /// row, which has no value to redact).
    HeaderValue,
    Url,
    /// Loopback / localhost — dim cyan.
    Loopback,
    /// Badge with severity-driven colour.
    Badge(BadgeSeverity),
    MatchOk,
    MatchNone,
    MatchDeny,
    SignalText,
    BodyMeta,
    BodyContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethodColour {
    Read,
    Write,
    Delete,
    Other,
}

impl HttpMethodColour {
    fn for_method(m: &HttpMethod) -> Self {
        match m {
            HttpMethod::Get | HttpMethod::Head => Self::Read,
            HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch => Self::Write,
            HttpMethod::Delete => Self::Delete,
            _ => Self::Other,
        }
    }
}

pub trait StyledWriter {
    fn write_styled(&mut self, s: &str, style: Style) -> io::Result<()>;

    fn plain(&mut self, s: &str) -> io::Result<()> {
        self.write_styled(s, Style::Plain)
    }

    fn newline(&mut self) -> io::Result<()> {
        self.plain("\n")
    }
}

/// `StyledWriter` impl that drops style tags. Use in tests and any
/// non-TTY context. Wraps any `io::Write`.
pub struct PlainWriter<W: Write>(pub W);

impl<W: Write> StyledWriter for PlainWriter<W> {
    fn write_styled(&mut self, s: &str, _style: Style) -> io::Result<()> {
        self.0.write_all(s.as_bytes())
    }
}

/// `StyledWriter` impl that emits ANSI escape codes via `anstyle`.
pub struct AnsiWriter<W: Write>(pub W);

impl<W: Write> StyledWriter for AnsiWriter<W> {
    fn write_styled(&mut self, s: &str, style: Style) -> io::Result<()> {
        let st = ansi_for(style);
        write!(self.0, "{st}{s}{st:#}")
    }
}

fn ansi_for(style: Style) -> anstyle::Style {
    use anstyle::AnsiColor::*;
    use anstyle::{Effects, Style as A};
    match style {
        Style::Plain => A::new(),
        Style::Header => A::new().bold(),
        Style::RuleLine => A::new().fg_color(Some(BrightBlack.into())),
        Style::Method(HttpMethodColour::Read) => A::new().fg_color(Some(Green.into())).bold(),
        Style::Method(HttpMethodColour::Write) => A::new().fg_color(Some(Yellow.into())).bold(),
        Style::Method(HttpMethodColour::Delete) => A::new().fg_color(Some(Red.into())).bold(),
        Style::Method(HttpMethodColour::Other) => A::new().fg_color(Some(Magenta.into())).bold(),
        Style::HeaderName => A::new().fg_color(Some(BrightBlue.into())),
        Style::RedactedHeader => A::new().fg_color(Some(Red.into())),
        Style::HeaderValue => A::new(),
        Style::Url => A::new()
            .fg_color(Some(Cyan.into()))
            .effects(Effects::UNDERLINE),
        Style::Loopback => A::new()
            .fg_color(Some(Cyan.into()))
            .effects(Effects::DIMMED),
        Style::Badge(BadgeSeverity::Info) => A::new().fg_color(Some(BrightBlack.into())),
        Style::Badge(BadgeSeverity::Warn) => A::new().fg_color(Some(Yellow.into())),
        Style::Badge(BadgeSeverity::Danger) => A::new().fg_color(Some(Red.into())).bold(),
        Style::MatchOk => A::new().fg_color(Some(Green.into())),
        Style::MatchNone => A::new().fg_color(Some(Yellow.into())),
        Style::MatchDeny => A::new().fg_color(Some(Red.into())).bold(),
        Style::SignalText => A::new().fg_color(Some(Yellow.into())),
        Style::BodyMeta => A::new().fg_color(Some(BrightBlack.into())),
        Style::BodyContent => A::new(),
    }
}

pub trait Renderer {
    /// Render the §8.5 layout for `p`. `outcome` populates the
    /// `Match:` line: `None` (no policy evaluated yet) renders the same
    /// "no rule" placeholder as `Some(Decision::Prompt)`; the daemon
    /// will distinguish those once it ships.
    fn render(
        &self,
        p: &ParsedCommand,
        outcome: Option<&Decision>,
        w: &mut dyn StyledWriter,
    ) -> io::Result<()>;
}

/// Default implementation of the §8.5 layout.
///
/// Phase 2 wires `outcome` through; Phase 1a tests that pass `None`
/// keep working unchanged.
pub struct DefaultRenderer;

impl Renderer for DefaultRenderer {
    fn render(
        &self,
        p: &ParsedCommand,
        outcome: Option<&Decision>,
        w: &mut dyn StyledWriter,
    ) -> io::Result<()> {
        const RULE: &str = " ─────────────────────────────────────────────────────────────────\n";

        w.write_styled(" vet  ", Style::Plain)?;
        w.write_styled(&sanitize_for_display(&p.command), Style::Header)?;
        w.newline()?;
        w.write_styled(RULE, Style::RuleLine)?;

        write_header_line(w, p)?;

        for (idx, eff) in p.effects.iter().enumerate() {
            match eff {
                Effect::HttpRequest(req) => {
                    write_http_headers(w, req)?;
                    if !matches!(req.body, Body::None) {
                        w.write_styled(RULE, Style::RuleLine)?;
                        write_http_body(w, req)?;
                    }
                    if let Some(auth) = &req.auth {
                        write_auth(w, auth)?;
                    }
                    let _ = idx;
                }
                Effect::FileWrite(fw) => write_file_write(w, fw)?,
                Effect::FileRead(fr) => write_file_read(w, fr)?,
                Effect::ProcessSpawn(ps) => write_process_spawn(w, ps)?,
                Effect::CredentialUse(_) | Effect::Network(_) => {
                    // No detail row in the §8.5 layout for these yet.
                }
            }
        }

        w.write_styled(RULE, Style::RuleLine)?;
        write_signals_line(w, &p.signals)?;
        write_match_line(w, outcome)?;
        Ok(())
    }
}

fn write_header_line(w: &mut dyn StyledWriter, p: &ParsedCommand) -> io::Result<()> {
    w.plain(" ")?;

    if let Some(Effect::HttpRequest(req)) = p.effects.first() {
        let colour = HttpMethodColour::for_method(&req.method);
        w.write_styled(
            &sanitize_for_display(req.method.as_str()),
            Style::Method(colour),
        )?;
        w.plain("  ")?;

        let url_str = req.url.as_str();
        let url_style = if is_loopback_url(&req.url) {
            Style::Loopback
        } else {
            Style::Url
        };
        w.write_styled(&sanitize_for_display(url_str), url_style)?;
    } else if !p.display_hints.primary_verb.is_empty() || !p.display_hints.primary_target.is_empty()
    {
        w.write_styled(
            &sanitize_for_display(&p.display_hints.primary_verb),
            Style::Header,
        )?;
        w.plain("  ")?;
        w.write_styled(
            &sanitize_for_display(&p.display_hints.primary_target),
            Style::Url,
        )?;
    } else {
        w.write_styled("(no effects)", Style::Plain)?;
    }

    for badge in &p.display_hints.badges {
        w.plain("  ")?;
        write_badge(w, badge)?;
    }
    w.newline()
}

fn write_badge(w: &mut dyn StyledWriter, b: &Badge) -> io::Result<()> {
    w.write_styled("[", Style::Badge(b.severity))?;
    w.write_styled(&sanitize_for_display(&b.label), Style::Badge(b.severity))?;
    w.write_styled("]", Style::Badge(b.severity))?;
    Ok(())
}

fn is_loopback_url(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn write_http_headers(w: &mut dyn StyledWriter, req: &HttpRequest) -> io::Result<()> {
    if req.headers.is_empty() {
        return Ok(());
    }
    let max_name = req
        .headers
        .iter()
        .map(|h| h.name.len())
        .max()
        .unwrap_or(0)
        .min(32);
    for h in &req.headers {
        write_header_row(w, h, max_name)?;
    }
    Ok(())
}

/// Render a single header row.
///
/// Every value is redacted to `••••<last4>` plus a dim `← redacted,
/// len N` suffix — the renderer treats all header values as
/// sensitive (see [`redact`] module docs). The §8.5 layout therefore
/// surfaces *which* headers a request carries, but never their
/// contents; an operator who needs the raw bytes runs the request
/// through a debugger or audits the upstream agent transcript.
fn write_header_row(w: &mut dyn StyledWriter, h: &Header, max_name: usize) -> io::Result<()> {
    w.plain("   ")?;
    let safe_name = sanitize_for_display(&h.name);
    w.write_styled(&format!("{safe_name}: "), Style::HeaderName)?;
    let pad = max_name.saturating_sub(h.name.len());
    if pad > 0 {
        w.plain(&" ".repeat(pad))?;
    }
    // Sanitise the *redacted* form too: the last-4 tail can
    // legitimately be a control byte and we want to scrub before
    // it reaches the writer.
    let redacted = redact::redact_value(&h.value);
    w.write_styled(&sanitize_for_display(&redacted), Style::RedactedHeader)?;
    w.write_styled(
        &format!("    ← redacted, len {}", h.value.len()),
        Style::BodyMeta,
    )?;
    w.newline()
}

fn write_http_body(w: &mut dyn StyledWriter, req: &HttpRequest) -> io::Result<()> {
    let (label, content): (String, Option<String>) = match &req.body {
        Body::None => return Ok(()),
        Body::Inline { bytes } => {
            let ct = content_type(&req.headers).unwrap_or("(no content-type)");
            (
                format!(
                    "Body  ({ct_safe}, {} B)",
                    bytes.len(),
                    ct_safe = sanitize_for_display(ct)
                ),
                String::from_utf8(bytes.clone()).ok(),
            )
        }
        Body::FromFile { path } => (
            format!(
                "Body  (from file: {})",
                sanitize_for_display(&path.display().to_string())
            ),
            None,
        ),
        Body::Form { fields } => (
            format!(
                "Body  (application/x-www-form-urlencoded, {} fields)",
                fields.len()
            ),
            None,
        ),
    };
    w.plain("   ")?;
    w.write_styled(&label, Style::BodyMeta)?;
    w.newline()?;
    if let Some(content) = content {
        w.plain("   ")?;
        w.write_styled(&sanitize_for_display(&content), Style::BodyContent)?;
        w.newline()?;
    }
    Ok(())
}

fn content_type(headers: &[Header]) -> Option<&str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-type"))
        .map(|h| h.value.as_str())
}

fn write_auth(w: &mut dyn StyledWriter, auth: &Auth) -> io::Result<()> {
    w.plain("   ")?;
    let (label, redacted) = match auth {
        Auth::Basic { user, .. } => (
            format!(
                "Auth: Basic user={}, password ••••",
                sanitize_for_display(user)
            ),
            true,
        ),
        Auth::Bearer { .. } => ("Auth: Bearer ••••".to_string(), true),
        Auth::Header { name } => (
            format!("Auth: via header {}", sanitize_for_display(name)),
            true,
        ),
        Auth::Netrc => ("Auth: via ~/.netrc".to_string(), false),
    };
    let style = if redacted {
        Style::RedactedHeader
    } else {
        Style::HeaderValue
    };
    w.write_styled(&label, style)?;
    w.newline()
}

fn write_file_write(w: &mut dyn StyledWriter, fw: &FileWrite) -> io::Result<()> {
    w.plain("   ")?;
    w.write_styled(
        &format!(
            "File write → {}",
            sanitize_for_display(&fw.path.display().to_string())
        ),
        Style::HeaderName,
    )?;
    w.newline()
}

fn write_file_read(w: &mut dyn StyledWriter, fr: &FileRead) -> io::Result<()> {
    w.plain("   ")?;
    w.write_styled(
        &format!(
            "File read ← {}",
            sanitize_for_display(&fr.path.display().to_string())
        ),
        Style::HeaderName,
    )?;
    w.newline()
}

fn write_process_spawn(w: &mut dyn StyledWriter, ps: &ProcessSpawn) -> io::Result<()> {
    w.plain("   ")?;
    w.write_styled(
        &format!("Process spawn: `{}`", sanitize_for_display(&ps.command)),
        Style::HeaderName,
    )?;
    w.newline()
}

fn write_signals_line(w: &mut dyn StyledWriter, signals: &[RiskSignal]) -> io::Result<()> {
    w.plain("   ")?;
    w.write_styled("Risk signals: ", Style::Header)?;
    if signals.is_empty() {
        w.write_styled("none", Style::SignalText)?;
    } else {
        let names: Vec<&'static str> = signals.iter().map(|s| signal_kind_label(s.kind)).collect();
        w.write_styled(&names.join(", "), Style::SignalText)?;
    }
    w.newline()
}

fn write_match_line(w: &mut dyn StyledWriter, outcome: Option<&Decision>) -> io::Result<()> {
    w.plain("   ")?;
    w.write_styled("Match:        ", Style::Header)?;
    match outcome {
        Some(Decision::Allow { rule_id, scope }) => {
            w.write_styled(
                &format!(
                    "matched rule {} ({})",
                    sanitize_for_display(rule_id),
                    scope_label(scope)
                ),
                Style::MatchOk,
            )?;
        }
        Some(Decision::Deny { rule_id, scope }) => {
            w.write_styled(
                &format!(
                    "denylist {} ({})",
                    sanitize_for_display(rule_id),
                    scope_label(scope)
                ),
                Style::MatchDeny,
            )?;
        }
        Some(Decision::Prompt) | None => {
            w.write_styled("no rule", Style::MatchNone)?;
        }
    }
    w.newline()
}

fn scope_label(scope: &Scope) -> &'static str {
    scope.as_str()
}

/// Short slug for `k` used by the §8.5 `Risk signals:` line and by
/// the macOS popover's header chips. Stable across releases — agents
/// and downstream tooling parse it.
pub fn signal_kind_label(k: SignalKind) -> &'static str {
    match k {
        SignalKind::WriteMethod => "write-method",
        SignalKind::AuthHeader => "auth-header",
        SignalKind::InsecureTls => "insecure-tls",
        SignalKind::NonStandardPort => "non-standard-port",
        SignalKind::IdnHost => "idn-host",
        SignalKind::RawIpLiteral => "raw-ip-literal",
        SignalKind::FileOutsideCwd => "file-outside-cwd",
        SignalKind::FileReadOutsideCwd => "file-read-outside-cwd",
        SignalKind::PipeToShell => "pipe-to-shell",
        SignalKind::UnknownHost => "unknown-host",
        SignalKind::InsecureFlag => "insecure-flag",
        SignalKind::ResolveOverride => "resolve-override",
        SignalKind::CacertOverride => "cacert-override",
        SignalKind::UnixSocket => "unix-socket",
        SignalKind::ClientCertificate => "client-certificate",
        SignalKind::RemoteHeaderName => "remote-header-name",
        SignalKind::CreateDirs => "create-dirs",
        SignalKind::FollowRedirects => "follow-redirects",
    }
}

#[cfg(test)]
#[path = "../tests/render.rs"]
mod tests;
