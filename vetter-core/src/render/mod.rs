//! Generic renderer that walks `ParsedCommand.effects` to produce the
//! styled summary block from `plans/Overview.md` §8.5.
//!
//! The renderer is command-agnostic: it never branches on
//! `ParsedCommand.command`. ANSI colouring is the writer's
//! responsibility — the renderer emits styled chunks via [`StyledWriter`]
//! and the binary chooses [`PlainWriter`] (tests, redirected stderr) or
//! [`AnsiWriter`] (TTY).

use std::io::{self, Write};

use crate::matcher::{Decision, Scope};
use crate::parsers::{
    Auth, Badge, BadgeSeverity, Body, Effect, FileRead, FileWrite, Header, HttpMethod, HttpRequest,
    ParsedCommand, ProcessSpawn,
};
use crate::signals::{RiskSignal, SignalKind};

mod redact;

pub use redact::is_secret_header;

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
    RedactedHeader,
    /// Header value rendered in full (non-secret).
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
        w.write_styled(&p.command, Style::Header)?;
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
        w.write_styled(req.method.as_str(), Style::Method(colour))?;
        w.plain("  ")?;

        let url_str = req.url.as_str();
        let url_style = if is_loopback_url(&req.url) {
            Style::Loopback
        } else {
            Style::Url
        };
        w.write_styled(url_str, url_style)?;
    } else if !p.display_hints.primary_verb.is_empty() || !p.display_hints.primary_target.is_empty()
    {
        w.write_styled(&p.display_hints.primary_verb, Style::Header)?;
        w.plain("  ")?;
        w.write_styled(&p.display_hints.primary_target, Style::Url)?;
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
    w.write_styled(&b.label, Style::Badge(b.severity))?;
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

fn write_header_row(w: &mut dyn StyledWriter, h: &Header, max_name: usize) -> io::Result<()> {
    w.plain("   ")?;
    let secret = redact::is_secret_header(&h.name);
    w.write_styled(&format!("{}: ", h.name), Style::HeaderName)?;
    let pad = max_name.saturating_sub(h.name.len());
    if pad > 0 {
        w.plain(&" ".repeat(pad))?;
    }
    if secret {
        let redacted = redact::redact_value(&h.value);
        w.write_styled(&redacted, Style::RedactedHeader)?;
        w.write_styled(
            &format!("    ← redacted, len {}", h.value.len()),
            Style::BodyMeta,
        )?;
    } else {
        w.write_styled(&h.value, Style::HeaderValue)?;
    }
    w.newline()
}

fn write_http_body(w: &mut dyn StyledWriter, req: &HttpRequest) -> io::Result<()> {
    let (label, content): (String, Option<String>) = match &req.body {
        Body::None => return Ok(()),
        Body::Inline { bytes } => {
            let ct = content_type(&req.headers).unwrap_or("(no content-type)");
            (
                format!("Body  ({ct}, {} B)", bytes.len()),
                String::from_utf8(bytes.clone()).ok(),
            )
        }
        Body::FromFile { path } => (format!("Body  (from file: {})", path.display()), None),
        Body::FromStdin { digest, len } => (
            format!("Body  (from stdin, {len} B, sha256 {})", digest.as_str()),
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
        w.write_styled(&content, Style::BodyContent)?;
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
        Auth::Basic { user, .. } => (format!("Auth: Basic user={user}, password ••••"), true),
        Auth::Bearer { .. } => ("Auth: Bearer ••••".to_string(), true),
        Auth::Header { name } => (format!("Auth: via header {name}"), true),
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
        &format!("File write → {}", fw.path.display()),
        Style::HeaderName,
    )?;
    w.newline()
}

fn write_file_read(w: &mut dyn StyledWriter, fr: &FileRead) -> io::Result<()> {
    w.plain("   ")?;
    w.write_styled(
        &format!("File read ← {}", fr.path.display()),
        Style::HeaderName,
    )?;
    w.newline()
}

fn write_process_spawn(w: &mut dyn StyledWriter, ps: &ProcessSpawn) -> io::Result<()> {
    w.plain("   ")?;
    w.write_styled(
        &format!("Process spawn: `{}`", ps.command),
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
                &format!("matched rule {rule_id} ({})", scope_label(scope)),
                Style::MatchOk,
            )?;
        }
        Some(Decision::Deny { rule_id, scope }) => {
            w.write_styled(
                &format!("denylist {rule_id} ({})", scope_label(scope)),
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

fn signal_kind_label(k: SignalKind) -> &'static str {
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
        SignalKind::InsecureFlag => "insecure-flag",
        SignalKind::ResolveOverride => "resolve-override",
        SignalKind::CacertOverride => "cacert-override",
        SignalKind::UnixSocket => "unix-socket",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::{
        Body, DisplayHints, Effect, Header, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy,
    };
    use url::Url;

    fn render_to_string(p: &ParsedCommand) -> String {
        render_to_string_with(p, None)
    }

    fn render_to_string_with(p: &ParsedCommand, outcome: Option<&Decision>) -> String {
        let mut buf = Vec::<u8>::new();
        DefaultRenderer
            .render(p, outcome, &mut PlainWriter(&mut buf))
            .expect("render");
        String::from_utf8(buf).expect("utf-8")
    }

    fn pc_get_with_headers(headers: Vec<(&str, &str)>) -> ParsedCommand {
        ParsedCommand {
            command: "noop".into(),
            argv: vec!["noop".into()],
            cwd: None,
            stdin_digest: None,
            effects: vec![Effect::HttpRequest(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse("https://example.test/").unwrap(),
                headers: headers
                    .into_iter()
                    .map(|(n, v)| Header {
                        name: n.into(),
                        value: v.into(),
                    })
                    .collect(),
                body: Body::None,
                auth: None,
                tls: TlsPolicy::Strict,
                follow_redirects: false,
                proxy: None,
            })],
            signals: vec![],
            display_hints: DisplayHints::default(),
            extras: serde_json::Value::Null,
        }
    }

    #[test]
    fn authorization_header_is_redacted_in_plain_output() {
        let p = pc_get_with_headers(vec![("Authorization", "Bearer abcdef1234567890wxyzf3a2")]);
        let out = render_to_string(&p);
        assert!(
            !out.contains("abcdef1234567890wxyzf3a2"),
            "raw token leaked: {out}"
        );
        assert!(out.contains("••••"), "missing redaction marker: {out}");
        assert!(out.contains("f3a2"), "missing last-4: {out}");
    }

    #[test]
    fn cookie_xapikey_proxyauth_xtoken_are_all_redacted() {
        for h in [
            "Cookie",
            "X-Api-Key",
            "Proxy-Authorization",
            "X-Vault-Token",
        ] {
            let p = pc_get_with_headers(vec![(h, "supersecret-value-1234")]);
            let out = render_to_string(&p);
            assert!(
                !out.contains("supersecret-value-1234"),
                "header {h} not redacted: {out}"
            );
        }
    }

    #[test]
    fn benign_headers_are_shown_in_full() {
        let p = pc_get_with_headers(vec![
            ("Content-Type", "application/json"),
            ("Accept", "*/*"),
            ("User-Agent", "curl/8.4.0"),
        ]);
        let out = render_to_string(&p);
        assert!(out.contains("application/json"));
        assert!(out.contains("*/*"));
        assert!(out.contains("curl/8.4.0"));
    }

    #[test]
    fn no_rule_match_line_is_present() {
        let p = pc_get_with_headers(vec![]);
        let out = render_to_string(&p);
        assert!(out.contains("Match:"));
        assert!(out.contains("no rule"));
    }

    #[test]
    fn long_url_is_not_truncated() {
        let long_path =
            "/repos/foo/bar/issues/12345?state=open&labels=needs-review&since=2026-01-01T00:00:00Z";
        let url = format!("https://api.github.com{long_path}");
        let p = ParsedCommand {
            command: "noop".into(),
            argv: vec![],
            cwd: None,
            stdin_digest: None,
            effects: vec![Effect::HttpRequest(HttpRequest {
                method: HttpMethod::Get,
                url: Url::parse(&url).unwrap(),
                headers: vec![],
                body: Body::None,
                auth: None,
                tls: TlsPolicy::Strict,
                follow_redirects: false,
                proxy: None,
            })],
            signals: vec![],
            display_hints: DisplayHints::default(),
            extras: serde_json::Value::Null,
        };
        let out = render_to_string(&p);
        assert!(out.contains(long_path), "path was truncated: {out}");
    }

    #[test]
    fn ansi_writer_emits_escape_codes_for_styled_chunks() {
        let p = pc_get_with_headers(vec![("Content-Type", "application/json")]);
        let mut buf = Vec::<u8>::new();
        DefaultRenderer
            .render(&p, None, &mut AnsiWriter(&mut buf))
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\x1b["), "no ANSI escape in styled output");
    }

    #[test]
    fn match_line_dispatches_on_decision() {
        let p = pc_get_with_headers(vec![]);

        let allow = Decision::Allow {
            rule_id: "github-readonly".into(),
            scope: Scope::Project,
        };
        let out = render_to_string_with(&p, Some(&allow));
        assert!(out.contains("matched rule github-readonly"), "{out}");
        assert!(out.contains("(project)"), "{out}");

        let deny = Decision::Deny {
            rule_id: "no-prod-writes".into(),
            scope: Scope::Denylist,
        };
        let out = render_to_string_with(&p, Some(&deny));
        assert!(out.contains("denylist no-prod-writes"), "{out}");
        assert!(out.contains("(denylist)"), "{out}");

        let prompt = Decision::Prompt;
        let out = render_to_string_with(&p, Some(&prompt));
        assert!(out.contains("no rule"), "{out}");
    }
}
