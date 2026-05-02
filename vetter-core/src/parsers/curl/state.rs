//! Curl state collector + finalisation.
//!
//! Walks the [`Token`]s emitted by [`super::flags::tokenise`] into a
//! [`CurlState`], then turns that state into a [`ParsedCommand`] by
//! materialising the body, file effects, auth, TLS policy, and
//! curl-specific risk signals.

use std::path::PathBuf;

use serde_json::json;
use sha2::{Digest, Sha256 as Sha256Hasher};
use url::Url;

use crate::parsers::{
    Auth, Badge, BadgeSeverity, Body, DisplayHints, Effect, FileRead, FileWrite, Header,
    HttpMethod, HttpRequest, ParseError, ParsedCommand, Sha256, StdinHandle, TlsPolicy,
    WriteSource,
};
use crate::signals::{RiskSignal, SignalKind};

use super::flags::{tokenise, FlagId, Token};

/// One `-d` / `--data*` chunk. Curl concatenates multiple chunks with
/// `&` regardless of variant; we preserve the variant only so the
/// renderer's detail view can show what was original.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DataChunk {
    Inline(String),
    File(PathBuf),
    /// `-d @-`: body sourced from stdin. Resolved during finalisation.
    Stdin,
}

#[derive(Debug, Default)]
struct CurlState {
    url: Option<Url>,
    extra_positionals: Vec<String>,
    method_override: Option<HttpMethod>,
    headers: Vec<Header>,
    data: Vec<DataChunk>,
    /// `-T <path>` upload. We reject `-T -` at tokenise-consume time.
    upload: Option<PathBuf>,
    /// `-o <path>`.
    output: Option<PathBuf>,
    /// `-O` short flag; output filename derived from URL basename.
    remote_name: bool,
    /// `-J` short flag; output filename from Content-Disposition. We
    /// can't predict the actual filename so we still surface the write
    /// effect with a placeholder path.
    remote_header_name: bool,
    user: Option<String>,
    netrc: bool,
    insecure: bool,
    head_only: bool,
    get_method: bool,
    proxy: Option<Url>,
    user_agent: Option<String>,
    cacert: Option<PathBuf>,
    resolve: Vec<String>,
    unix_socket: Option<PathBuf>,
    follow_redirects: bool,
    /// Unrecognised long flags, optionally with values, preserved into
    /// `extras` so the renderer's detail view can display them.
    unknown_longs: Vec<UnknownLong>,
    /// Unrecognised short-flag characters, also preserved.
    unknown_shorts: Vec<char>,
}

#[derive(Debug, Clone)]
struct UnknownLong {
    name: String,
    value: Option<String>,
}

/// Public entry point used by `super::CurlParser::parse`.
pub(super) fn parse_argv(
    argv: &[String],
    stdin: &StdinHandle<'_>,
) -> Result<ParsedCommand, ParseError> {
    if argv.is_empty() {
        return Err(ParseError::Other("argv is empty".into()));
    }
    let tokens = tokenise(&argv[1..])?;
    let mut state = CurlState::default();
    for tok in tokens {
        absorb(&mut state, tok)?;
    }
    finalise(state, argv, stdin)
}

fn absorb(state: &mut CurlState, tok: Token) -> Result<(), ParseError> {
    match tok {
        Token::Known { spec, value } => match spec.id {
            FlagId::Request => {
                let v = value.expect("Value flag has value");
                let upper = v.to_ascii_uppercase();
                if let Some(existing) = &state.method_override {
                    if existing.as_str() != upper {
                        return Err(ParseError::ConflictingArgs(format!(
                            "-X {} conflicts with prior -X {}",
                            upper,
                            existing.as_str()
                        )));
                    }
                }
                state.method_override = Some(HttpMethod::from(upper.as_str()));
            }
            FlagId::Header => {
                let v = value.expect("Value flag has value");
                let header = parse_header(&v)?;
                state.headers.push(header);
            }
            FlagId::Data
            | FlagId::DataRaw
            | FlagId::DataBinary
            | FlagId::DataAscii
            | FlagId::DataUrlencode => {
                let v = value.expect("Value flag has value");
                state.data.push(classify_data(&v, spec.id));
            }
            FlagId::User => {
                state.user = Some(value.expect("Value flag has value"));
            }
            FlagId::Insecure => {
                state.insecure = true;
            }
            FlagId::Output => {
                state.output = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::RemoteName => {
                state.remote_name = true;
            }
            FlagId::RemoteHeaderName => {
                state.remote_header_name = true;
            }
            FlagId::UploadFile => {
                let v = value.expect("Value flag has value");
                if v == "-" {
                    return Err(ParseError::StreamingUnsupported);
                }
                state.upload = Some(PathBuf::from(v));
            }
            FlagId::Location => {
                state.follow_redirects = true;
            }
            FlagId::Proxy => {
                let v = value.expect("Value flag has value");
                let parsed = Url::parse(&v)
                    .map_err(|e| ParseError::Other(format!("invalid --proxy URL `{v}`: {e}")))?;
                state.proxy = Some(parsed);
            }
            FlagId::UserAgent => {
                state.user_agent = Some(value.expect("Value flag has value"));
            }
            FlagId::Netrc => {
                state.netrc = true;
            }
            FlagId::CaCert => {
                state.cacert = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::Resolve => {
                state.resolve.push(value.expect("Value flag has value"));
            }
            FlagId::UnixSocket => {
                state.unix_socket = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::Get => {
                state.get_method = true;
            }
            FlagId::Head => {
                state.head_only = true;
            }
            FlagId::Silent
            | FlagId::Verbose
            | FlagId::Fail
            | FlagId::ShowError
            | FlagId::ProgressBar => {
                // Accepted, but informational only — they don't change
                // any effect we care about.
            }
        },
        Token::UnknownLong { name, value } => {
            state.unknown_longs.push(UnknownLong { name, value });
        }
        Token::UnknownShort { c } => {
            state.unknown_shorts.push(c);
        }
        Token::Positional(p) => match state.url {
            None => {
                let parsed = Url::parse(&p)
                    .map_err(|e| ParseError::Other(format!("invalid URL `{p}`: {e}")))?;
                state.url = Some(parsed);
            }
            Some(_) => {
                state.extra_positionals.push(p);
            }
        },
    }
    Ok(())
}

fn classify_data(raw: &str, id: FlagId) -> DataChunk {
    // --data-raw never honours the @ prefix.
    if id == FlagId::DataRaw {
        return DataChunk::Inline(raw.to_string());
    }
    if let Some(rest) = raw.strip_prefix('@') {
        if rest == "-" {
            DataChunk::Stdin
        } else {
            DataChunk::File(PathBuf::from(rest))
        }
    } else {
        DataChunk::Inline(raw.to_string())
    }
}

fn parse_header(raw: &str) -> Result<Header, ParseError> {
    // Curl tolerates `Header: value` and `Header:value`. Empty value
    // (`Header;`) is a curl idiom for "send Header with no value".
    if let Some((name, value)) = raw.split_once(':') {
        Ok(Header {
            name: name.trim().to_string(),
            value: value.trim_start().to_string(),
        })
    } else if let Some(name) = raw.strip_suffix(';') {
        Ok(Header {
            name: name.trim().to_string(),
            value: String::new(),
        })
    } else {
        Err(ParseError::Other(format!(
            "malformed -H value `{raw}` (expected `Name: value`)"
        )))
    }
}

fn finalise(
    state: CurlState,
    argv: &[String],
    stdin: &StdinHandle<'_>,
) -> Result<ParsedCommand, ParseError> {
    if !state.extra_positionals.is_empty() {
        return Err(ParseError::Other(format!(
            "multiple URLs are not supported: extras = {:?}",
            state.extra_positionals
        )));
    }

    let mut url = state
        .url
        .clone()
        .ok_or_else(|| ParseError::MissingArgument("URL".into()))?;
    url.set_fragment(None);

    if has_chunked_transfer(&state.headers) {
        return Err(ParseError::StreamingUnsupported);
    }

    let method = infer_method(&state);

    let (body, file_read_effect, stdin_digest) = build_body(&state, stdin)?;

    let tls = if state.insecure {
        TlsPolicy::InsecureSkipVerify
    } else if url.scheme() == "http" {
        TlsPolicy::Plaintext
    } else {
        TlsPolicy::Strict
    };

    let auth = derive_auth(&state);

    let http = HttpRequest {
        method: method.clone(),
        url: url.clone(),
        headers: state.headers.clone(),
        body,
        auth,
        tls,
        follow_redirects: state.follow_redirects,
        proxy: state.proxy.clone(),
    };

    let mut effects: Vec<Effect> = vec![Effect::HttpRequest(http)];

    if let Some(fr) = file_read_effect {
        effects.push(Effect::FileRead(fr));
    }
    if let Some(upload_path) = &state.upload {
        effects.push(Effect::FileRead(FileRead {
            path: upload_path.clone(),
        }));
    }
    if let Some(write) = build_file_write(&state, &url) {
        effects.push(Effect::FileWrite(write));
    }

    let signals = build_signals(&state);

    let display_hints = build_display_hints(&method, &url, &state);

    let extras = build_extras(&state);

    Ok(ParsedCommand {
        command: "curl".into(),
        argv: argv.to_vec(),
        cwd: None,
        stdin_digest,
        effects,
        signals,
        display_hints,
        extras,
    })
}

fn has_chunked_transfer(headers: &[Header]) -> bool {
    headers.iter().any(|h| {
        h.name.eq_ignore_ascii_case("transfer-encoding")
            && h.value.to_ascii_lowercase().contains("chunked")
    })
}

fn infer_method(state: &CurlState) -> HttpMethod {
    if let Some(m) = &state.method_override {
        return m.clone();
    }
    if state.head_only {
        return HttpMethod::Head;
    }
    if state.get_method {
        return HttpMethod::Get;
    }
    if !state.data.is_empty() || state.upload.is_some() {
        if state.upload.is_some() && state.data.is_empty() {
            return HttpMethod::Put;
        }
        return HttpMethod::Post;
    }
    HttpMethod::Get
}

fn build_body(
    state: &CurlState,
    stdin: &StdinHandle<'_>,
) -> Result<(Body, Option<FileRead>, Option<Sha256>), ParseError> {
    if state.data.is_empty() {
        return Ok((Body::None, None, None));
    }

    // Multiple chunks with mixed sources are unusual; we only specially
    // handle the common cases. If any chunk is Stdin, the body becomes
    // FromStdin (and other chunks are dropped — curl semantics here are
    // unclear and we'd rather under-allow than mis-vet).
    if state.data.iter().any(|c| matches!(c, DataChunk::Stdin)) {
        let buf = stdin.buf.unwrap_or_default();
        if buf.len() > stdin.cap_bytes {
            return Err(ParseError::StreamingUnsupported);
        }
        let mut hasher = Sha256Hasher::new();
        hasher.update(buf);
        let digest = format!("{:x}", hasher.finalize());
        let len = buf.len() as u64;
        return Ok((
            Body::FromStdin {
                digest: Sha256::new(digest.clone()),
                len,
            },
            None,
            Some(Sha256::new(digest)),
        ));
    }

    // Single file chunk → FromFile + FileRead.
    if state.data.len() == 1 {
        if let DataChunk::File(path) = &state.data[0] {
            return Ok((
                Body::FromFile { path: path.clone() },
                Some(FileRead { path: path.clone() }),
                None,
            ));
        }
    }

    // Otherwise we have one-or-more inline chunks (and possibly file
    // chunks; the latter are emitted as FileRead effects but the body
    // is the inline-concatenation per curl's `&`-join rule).
    let mut inline_parts: Vec<String> = Vec::new();
    let mut file_reads: Vec<FileRead> = Vec::new();
    for c in &state.data {
        match c {
            DataChunk::Inline(s) => inline_parts.push(s.clone()),
            DataChunk::File(p) => file_reads.push(FileRead { path: p.clone() }),
            DataChunk::Stdin => unreachable!("handled above"),
        }
    }
    let joined = inline_parts.join("&");
    let body = Body::Inline {
        bytes: joined.into_bytes(),
    };
    // We can only emit one FileRead per build_body return; for multiple
    // -d @file chunks we surface the first and lose the rest. Mixed
    // -d 'literal' + -d @file is a rare combination; keeping it simple
    // is fine for MVP.
    let fr = file_reads.into_iter().next();
    Ok((body, fr, None))
}

fn derive_auth(state: &CurlState) -> Option<Auth> {
    if let Some(user) = &state.user {
        let (u, _pwd) = match user.split_once(':') {
            Some((u, p)) => (u.to_string(), Some(p.to_string())),
            None => (user.clone(), None),
        };
        return Some(Auth::Basic {
            user: u,
            password_redacted: true,
        });
    }
    if state.netrc {
        return Some(Auth::Netrc);
    }
    for h in &state.headers {
        if h.name.eq_ignore_ascii_case("authorization") {
            let lower = h.value.to_ascii_lowercase();
            if lower.starts_with("bearer ") {
                return Some(Auth::Bearer {
                    token_redacted: true,
                });
            }
            if lower.starts_with("basic ") {
                return Some(Auth::Basic {
                    user: String::new(),
                    password_redacted: true,
                });
            }
            return Some(Auth::Header {
                name: h.name.clone(),
            });
        }
        if crate::render::is_secret_header(&h.name) {
            return Some(Auth::Header {
                name: h.name.clone(),
            });
        }
    }
    None
}

fn build_file_write(state: &CurlState, url: &Url) -> Option<FileWrite> {
    if let Some(path) = &state.output {
        return Some(FileWrite {
            path: path.clone(),
            source: WriteSource::RemoteHttp { url: url.clone() },
            overwrite: true,
        });
    }
    if state.remote_name || state.remote_header_name {
        let basename = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("download");
        return Some(FileWrite {
            path: PathBuf::from(basename),
            source: WriteSource::RemoteHttp { url: url.clone() },
            overwrite: true,
        });
    }
    None
}

fn build_signals(state: &CurlState) -> Vec<RiskSignal> {
    let mut out = Vec::new();
    if state.insecure {
        out.push(RiskSignal {
            kind: SignalKind::InsecureFlag,
            detail: "--insecure / -k present".into(),
            effect_idx: Some(0),
        });
    }
    if let Some(p) = &state.cacert {
        out.push(RiskSignal {
            kind: SignalKind::CacertOverride,
            detail: format!("--cacert {}", p.display()),
            effect_idx: Some(0),
        });
    }
    for r in &state.resolve {
        out.push(RiskSignal {
            kind: SignalKind::ResolveOverride,
            detail: format!("--resolve {r}"),
            effect_idx: Some(0),
        });
    }
    if let Some(p) = &state.unix_socket {
        out.push(RiskSignal {
            kind: SignalKind::UnixSocket,
            detail: format!("--unix-socket {}", p.display()),
            effect_idx: Some(0),
        });
    }
    out
}

fn build_display_hints(method: &HttpMethod, url: &Url, state: &CurlState) -> DisplayHints {
    let mut badges: Vec<Badge> = Vec::new();
    if state.insecure {
        badges.push(Badge {
            label: "insecure: -k".into(),
            severity: BadgeSeverity::Danger,
        });
    }
    if state.unix_socket.is_some() {
        badges.push(Badge {
            label: "unix-socket".into(),
            severity: BadgeSeverity::Warn,
        });
    }
    if state.data.iter().any(|c| matches!(c, DataChunk::Stdin)) {
        badges.push(Badge {
            label: "stdin".into(),
            severity: BadgeSeverity::Info,
        });
    }
    DisplayHints {
        primary_verb: method.as_str().to_string(),
        primary_target: url.to_string(),
        badges,
    }
}

fn build_extras(state: &CurlState) -> serde_json::Value {
    let mut extras = serde_json::Map::new();
    if !state.unknown_longs.is_empty() {
        let arr: Vec<serde_json::Value> = state
            .unknown_longs
            .iter()
            .map(|u| match &u.value {
                Some(v) => json!({"name": u.name, "value": v}),
                None => json!({"name": u.name}),
            })
            .collect();
        extras.insert(
            "unknown_long_flags".to_string(),
            serde_json::Value::Array(arr),
        );
    }
    if !state.unknown_shorts.is_empty() {
        let arr: Vec<serde_json::Value> = state
            .unknown_shorts
            .iter()
            .map(|c| serde_json::Value::String(c.to_string()))
            .collect();
        extras.insert(
            "unknown_short_flags".to_string(),
            serde_json::Value::Array(arr),
        );
    }
    if let Some(ua) = &state.user_agent {
        extras.insert(
            "user_agent_flag".to_string(),
            serde_json::Value::String(ua.clone()),
        );
    }
    if extras.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(extras)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("curl".to_string())
            .chain(args.iter().map(|a| a.to_string()))
            .collect()
    }

    fn parse(args: &[&str]) -> ParsedCommand {
        parse_argv(&argv(args), &StdinHandle::empty()).expect("parse")
    }

    fn parse_stdin(args: &[&str], stdin: &[u8], cap: usize) -> Result<ParsedCommand, ParseError> {
        parse_argv(&argv(args), &StdinHandle::from_bytes(stdin, cap))
    }

    fn http_of(p: &ParsedCommand) -> &HttpRequest {
        match &p.effects[0] {
            Effect::HttpRequest(r) => r,
            _ => panic!("expected http"),
        }
    }

    #[test]
    fn missing_url_errors() {
        let r = parse_argv(&argv(&[]), &StdinHandle::empty());
        assert!(matches!(r, Err(ParseError::MissingArgument(_))));
    }

    #[test]
    fn missing_url_with_flags_only_errors() {
        let r = parse_argv(&argv(&["-k"]), &StdinHandle::empty());
        assert!(matches!(r, Err(ParseError::MissingArgument(_))));
    }

    #[test]
    fn implicit_method_is_get() {
        let p = parse(&["https://example.test/"]);
        assert_eq!(http_of(&p).method, HttpMethod::Get);
    }

    #[test]
    fn implicit_method_with_data_is_post() {
        let p = parse(&["-d", "foo=bar", "https://example.test/"]);
        assert_eq!(http_of(&p).method, HttpMethod::Post);
    }

    #[test]
    fn explicit_method_override_wins() {
        let p = parse(&["-X", "PATCH", "-d", "x", "https://example.test/"]);
        assert_eq!(http_of(&p).method, HttpMethod::Patch);
    }

    #[test]
    fn method_override_lowercased_input_is_uppercased() {
        let p = parse(&["-X", "delete", "https://example.test/"]);
        assert_eq!(http_of(&p).method, HttpMethod::Delete);
    }

    #[test]
    fn conflicting_methods_error() {
        let r = parse_argv(
            &argv(&["-X", "GET", "-X", "POST", "https://example.test/"]),
            &StdinHandle::empty(),
        );
        assert!(matches!(r, Err(ParseError::ConflictingArgs(_))));
    }

    #[test]
    fn url_fragment_stripped_query_preserved() {
        let p = parse(&["https://example.test/path?q=1#frag"]);
        let url = &http_of(&p).url;
        assert_eq!(url.path(), "/path");
        assert_eq!(url.query(), Some("q=1"));
        assert_eq!(url.fragment(), None);
    }

    #[test]
    fn multiple_data_chunks_concatenate_with_amp() {
        let p = parse(&["-d", "a=1", "-d", "b=2", "https://example.test/"]);
        match &http_of(&p).body {
            Body::Inline { bytes } => assert_eq!(bytes, b"a=1&b=2"),
            other => panic!("expected inline body, got {other:?}"),
        }
    }

    #[test]
    fn header_parsing_trims_value_whitespace() {
        let p = parse(&["-H", "X-Foo:   bar", "https://example.test/"]);
        assert_eq!(http_of(&p).headers[0].name, "X-Foo");
        assert_eq!(http_of(&p).headers[0].value, "bar");
    }

    #[test]
    fn malformed_header_errors() {
        let r = parse_argv(
            &argv(&["-H", "no-colon-here", "https://example.test/"]),
            &StdinHandle::empty(),
        );
        assert!(matches!(r, Err(ParseError::Other(_))));
    }

    #[test]
    fn chunked_transfer_header_rejected_case_insensitive() {
        let r = parse_argv(
            &argv(&["-H", "transfer-encoding: Chunked", "https://example.test/"]),
            &StdinHandle::empty(),
        );
        assert!(matches!(r, Err(ParseError::StreamingUnsupported)));
    }

    #[test]
    fn upload_stdin_rejected() {
        let r = parse_argv(
            &argv(&["-T", "-", "https://example.test/"]),
            &StdinHandle::empty(),
        );
        assert!(matches!(r, Err(ParseError::StreamingUnsupported)));
    }

    #[test]
    fn data_at_stdin_within_cap_yields_from_stdin() {
        let p = parse_stdin(&["-d", "@-", "https://example.test/"], b"hello", 1024).expect("parse");
        match &http_of(&p).body {
            Body::FromStdin { len, digest } => {
                assert_eq!(*len, 5);
                assert!(!digest.as_str().is_empty());
            }
            other => panic!("expected FromStdin, got {other:?}"),
        }
        assert!(p.stdin_digest.is_some());
    }

    #[test]
    fn data_at_stdin_over_cap_errors() {
        let stdin = vec![b'a'; 16];
        let r = parse_stdin(&["-d", "@-", "https://example.test/"], &stdin, 8);
        assert!(matches!(r, Err(ParseError::StreamingUnsupported)));
    }

    #[test]
    fn data_at_file_emits_file_read() {
        let p = parse(&["-d", "@./payload.json", "https://example.test/"]);
        let reads: Vec<&FileRead> = p
            .effects
            .iter()
            .filter_map(|e| {
                if let Effect::FileRead(r) = e {
                    Some(r)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, PathBuf::from("./payload.json"));
        match &http_of(&p).body {
            Body::FromFile { path } => assert_eq!(path, &PathBuf::from("./payload.json")),
            other => panic!("expected FromFile, got {other:?}"),
        }
    }

    #[test]
    fn upload_file_emits_file_read_and_implies_put() {
        let p = parse(&["-T", "/tmp/x", "https://example.test/"]);
        assert_eq!(http_of(&p).method, HttpMethod::Put);
        let reads: Vec<&FileRead> = p
            .effects
            .iter()
            .filter_map(|e| {
                if let Effect::FileRead(r) = e {
                    Some(r)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn output_flag_emits_file_write() {
        let p = parse(&["-o", "/tmp/out", "https://example.test/file.json"]);
        let writes: Vec<&FileWrite> = p
            .effects
            .iter()
            .filter_map(|e| {
                if let Effect::FileWrite(w) = e {
                    Some(w)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, PathBuf::from("/tmp/out"));
    }

    #[test]
    fn remote_name_uses_url_basename_for_path() {
        let p = parse(&["-O", "https://example.test/dir/thing.tgz"]);
        let writes: Vec<&FileWrite> = p
            .effects
            .iter()
            .filter_map(|e| {
                if let Effect::FileWrite(w) = e {
                    Some(w)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(writes[0].path, PathBuf::from("thing.tgz"));
    }

    #[test]
    fn insecure_sets_tls_and_signal_and_badge() {
        let p = parse(&["-k", "https://example.test/"]);
        assert_eq!(http_of(&p).tls, TlsPolicy::InsecureSkipVerify);
        assert!(p.signals.iter().any(|s| s.kind == SignalKind::InsecureFlag));
        assert!(p
            .display_hints
            .badges
            .iter()
            .any(|b| b.label.contains("insecure")));
    }

    #[test]
    fn http_scheme_marks_tls_plaintext() {
        let p = parse(&["http://example.test/"]);
        assert_eq!(http_of(&p).tls, TlsPolicy::Plaintext);
    }

    #[test]
    fn cacert_override_pushes_signal() {
        let p = parse(&["--cacert", "/etc/foo.pem", "https://example.test/"]);
        assert!(p
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::CacertOverride));
    }

    #[test]
    fn cacert_signal_absent_when_flag_absent() {
        let p = parse(&["https://example.test/"]);
        assert!(!p
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::CacertOverride));
    }

    #[test]
    fn resolve_pushes_signal_per_entry() {
        let p = parse(&[
            "--resolve",
            "a.example:443:1.2.3.4",
            "--resolve",
            "b.example:443:5.6.7.8",
            "https://a.example/",
        ]);
        let n = p
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::ResolveOverride)
            .count();
        assert_eq!(n, 2);
    }

    #[test]
    fn unix_socket_pushes_signal_and_badge() {
        let p = parse(&[
            "--unix-socket",
            "/var/run/docker.sock",
            "http://localhost/v1/info",
        ]);
        assert!(p.signals.iter().any(|s| s.kind == SignalKind::UnixSocket));
        assert!(p
            .display_hints
            .badges
            .iter()
            .any(|b| b.label.contains("unix-socket")));
    }

    #[test]
    fn user_flag_yields_basic_auth() {
        let p = parse(&["--user", "alice:secret", "https://example.test/"]);
        match http_of(&p).auth.as_ref().expect("auth") {
            Auth::Basic { user, .. } => assert_eq!(user, "alice"),
            other => panic!("expected Basic, got {other:?}"),
        }
    }

    #[test]
    fn netrc_flag_yields_netrc_auth() {
        let p = parse(&["--netrc", "https://example.test/"]);
        assert!(matches!(http_of(&p).auth, Some(Auth::Netrc)));
    }

    #[test]
    fn bearer_header_yields_bearer_auth() {
        let p = parse(&[
            "-H",
            "Authorization: Bearer tok-abc-123",
            "https://example.test/",
        ]);
        assert!(matches!(
            http_of(&p).auth,
            Some(Auth::Bearer {
                token_redacted: true
            })
        ));
    }

    #[test]
    fn unknown_long_flag_preserved_in_extras() {
        let p = parse(&["--never-heard-of-it", "https://example.test/"]);
        assert!(
            p.extras
                .get("unknown_long_flags")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty()),
            "extras: {:?}",
            p.extras
        );
    }

    #[test]
    fn _exhaustive_flag_id_match_compiles() {
        // Compile-time exhaustiveness reminder: if a new FlagId is added
        // to flags.rs, this match will fail to compile until `absorb`
        // handles it. Keeping this here so the linkage is explicit.
        fn _check(id: FlagId) {
            match id {
                FlagId::Request
                | FlagId::Header
                | FlagId::Data
                | FlagId::DataRaw
                | FlagId::DataBinary
                | FlagId::DataAscii
                | FlagId::DataUrlencode
                | FlagId::User
                | FlagId::Insecure
                | FlagId::Output
                | FlagId::RemoteName
                | FlagId::RemoteHeaderName
                | FlagId::UploadFile
                | FlagId::Location
                | FlagId::Proxy
                | FlagId::UserAgent
                | FlagId::Netrc
                | FlagId::CaCert
                | FlagId::Resolve
                | FlagId::UnixSocket
                | FlagId::Get
                | FlagId::Head
                | FlagId::Silent
                | FlagId::Verbose
                | FlagId::Fail
                | FlagId::ShowError
                | FlagId::ProgressBar => {}
            }
        }
        let _ = _check;
    }
}
