//! Curl state collector + finalisation.
//!
//! Walks the [`Token`]s emitted by [`super::flags::tokenise`] into a
//! [`CurlState`], then turns that state into a [`ParsedCommand`] by
//! materialising the body, file effects, auth, TLS policy, and
//! curl-specific risk signals.

use std::path::{Component, Path, PathBuf};

use serde_json::json;
use url::Url;

use crate::parsers::{
    Auth, Badge, BadgeSeverity, Body, DisplayHints, Effect, FileRead, FileWrite, Header,
    HttpMethod, HttpRequest, ParseError, ParsedCommand, StdinHandle, TlsPolicy, WriteSource,
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
    /// `-d @-`: body would be sourced from stdin. Curl invocations that
    /// pipe their body in this way are rejected outright in
    /// [`build_body`] with [`ParseError::StreamingUnsupported`]: agents
    /// must materialise the bytes to a temp file and use `-d @file` so
    /// the daemon's audit log faithfully records what `vet` execs.
    Stdin,
}

/// One `-b` / `--cookie` occurrence. Curl accepts either inline
/// `key=value` pairs (sent as a `Cookie:` header) or `@file` to load
/// a Netscape-format cookie file. We keep them split so finalisation
/// can emit a [`FileRead`] for every file occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CookieChunk {
    Inline(String),
    File(PathBuf),
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
    /// `--output-dir <dir>`: prefix applied to relative `-o`/`-O`/`-J`
    /// paths before `cwd` resolution. Absolute `-o` paths ignore it.
    /// Last-occurrence-wins per curl.
    output_dir: Option<PathBuf>,
    /// `--no-clobber`: flip `FileWrite.overwrite` to `false`. Bool;
    /// curl has no positive form.
    no_clobber: bool,
    /// `--create-dirs`: curl will mkdir-p any missing components of
    /// the `FileWrite` path. We don't change the path itself; we
    /// just push a signal so the approver sees that writes can land
    /// arbitrarily deep below `--output-dir` / `-o`'s parent.
    create_dirs: bool,
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
    /// `--cert <cert[:password]>` path component (left of the optional
    /// `:password` suffix).
    cert: Option<PathBuf>,
    /// `--key <key>` private-key path.
    key: Option<PathBuf>,
    /// `--pubkey <key>` SSH public-key path.
    pubkey: Option<PathBuf>,
    /// `--cert-type <type>` (e.g. PEM, DER, ENG, P12). Not a path.
    cert_type: Option<String>,
    /// `--key-type <type>` (e.g. PEM, DER, ENG). Not a path.
    key_type: Option<String>,
    /// `--engine <name>` OpenSSL engine name. Not a path.
    engine: Option<String>,
    /// True if a key passphrase was supplied via either `--pass` or the
    /// optional `:password` suffix on `--cert`. The passphrase value
    /// itself is deliberately never stored — see absorb / build_extras.
    cert_password_supplied: bool,
    /// `-b` / `--cookie` occurrences. Each is either an inline
    /// `key=value` pair (sent as a `Cookie:` header by curl) or a
    /// path to a Netscape-format cookie file (`@file`). File entries
    /// surface as [`FileRead`] effects in finalise; inline entries
    /// only land in `extras.cookies_inline` for visibility.
    cookies: Vec<CookieChunk>,
    /// `-c` / `--cookie-jar <file>`: curl writes the in-memory jar
    /// here on exit. Curl honours only the most-recent occurrence so
    /// this is `Option`, not `Vec`.
    cookie_jar: Option<PathBuf>,
    /// `-D` / `--dump-header <file>`: response headers as received.
    /// `None` when the user passed `-` (stdout) or omitted the flag,
    /// so finalisation only emits a `FileWrite` for real paths.
    dump_header: Option<PathBuf>,
    /// `--trace <file>`: full hex+ASCII protocol trace. `None` for the
    /// `-` (stdout) and `%` (stderr) sentinels.
    trace: Option<PathBuf>,
    /// `--trace-ascii <file>`: ASCII-only protocol trace. Same `-` /
    /// `%` sentinels as `--trace`.
    trace_ascii: Option<PathBuf>,
    /// `--etag-save <file>`: write the response ETag to disk.
    etag_save: Option<PathBuf>,
    /// `--etag-compare <file>`: read an ETag from disk and add it as
    /// `If-None-Match` on the request.
    etag_compare: Option<PathBuf>,
    /// `-w` / `--write-out <fmt>`: only the `@file` form references a
    /// file the parser surfaces (as `FileRead`); bare format strings
    /// are informational and leave this `None`. `@-` (stdin) is
    /// rejected at absorb-time via `StreamingUnsupported`.
    write_out_file: Option<PathBuf>,
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
///
/// `cwd` is the agent's working directory as reported by
/// [`crate::parsers::EnvSnapshot::cwd`]. When present, all relative
/// `FileRead` / `FileWrite` paths produced by this parser are resolved
/// against it (and `.`/`..` segments collapsed) so downstream
/// [`crate::signals::analyze`] sees absolute paths and stops emitting
/// spurious `FileOutsideCwd` / `FileReadOutsideCwd` signals for things
/// like `-o ./out`. When `cwd` is `None` (the unit-test / corpus path)
/// the parser leaves paths verbatim.
pub(super) fn parse_argv(
    argv: &[String],
    stdin: &StdinHandle<'_>,
    cwd: Option<&Path>,
) -> Result<ParsedCommand, ParseError> {
    if argv.is_empty() {
        return Err(ParseError::Other("argv is empty".into()));
    }
    let tokens = tokenise(&argv[1..])?;
    let mut state = CurlState::default();
    for tok in tokens {
        absorb(&mut state, tok)?;
    }
    finalise(state, argv, stdin, cwd)
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
            FlagId::OutputDir => {
                state.output_dir = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::NoClobber => {
                state.no_clobber = true;
            }
            FlagId::CreateDirs => {
                state.create_dirs = true;
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
            FlagId::Config => {
                let v = value.expect("Value flag has value");
                return Err(ParseError::Other(format!(
                    "--config / -K {v} is not supported; vet refuses to run rather \
                     than mis-vet a config file that can override every other flag"
                )));
            }
            FlagId::Next => {
                return Err(ParseError::Other(
                    "--next / -: is not supported; vet refuses to run rather than \
                     only vet the first of multiple requests in one curl invocation"
                        .into(),
                ));
            }
            FlagId::Form => {
                let v = value.expect("Value flag has value");
                return Err(ParseError::Other(format!(
                    "--form / -F {v} is not supported; vet refuses to run rather \
                     than mis-vet a multipart form (use a smaller, parseable curl \
                     invocation, or extend the parser if multipart is genuinely \
                     needed for this workflow)"
                )));
            }
            FlagId::FormString => {
                let v = value.expect("Value flag has value");
                return Err(ParseError::Other(format!(
                    "--form-string {v} is not supported; vet refuses to run rather \
                     than mis-vet a multipart form (use a smaller, parseable curl \
                     invocation)"
                )));
            }
            FlagId::Cert => {
                // `--cert <cert[:password]>`. Split once on `:`; the
                // left half is the file path, the right half (if any)
                // is the passphrase. Store only the password-supplied
                // bit so the value cannot leak through extras / audit.
                let v = value.expect("Value flag has value");
                let (path, password_present) = match v.split_once(':') {
                    Some((p, _pw)) => (p.to_string(), true),
                    None => (v, false),
                };
                state.cert = Some(PathBuf::from(path));
                if password_present {
                    state.cert_password_supplied = true;
                }
            }
            FlagId::Key => {
                state.key = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::Pubkey => {
                state.pubkey = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::CertType => {
                state.cert_type = Some(value.expect("Value flag has value"));
            }
            FlagId::KeyType => {
                state.key_type = Some(value.expect("Value flag has value"));
            }
            FlagId::Engine => {
                state.engine = Some(value.expect("Value flag has value"));
            }
            FlagId::Pass => {
                // Deliberately drop the value: `--pass` is a key
                // passphrase. We surface only the boolean
                // `cert_password_supplied` flag in extras so a future
                // renderer cannot accidentally echo the passphrase.
                let _ = value.expect("Value flag has value");
                state.cert_password_supplied = true;
            }
            FlagId::Cookie => {
                let v = value.expect("Value flag has value");
                state.cookies.push(classify_cookie(&v)?);
            }
            FlagId::CookieJar => {
                state.cookie_jar = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::DumpHeader => {
                let v = value.expect("Value flag has value");
                state.dump_header = if v == "-" {
                    None
                } else {
                    Some(PathBuf::from(v))
                };
            }
            FlagId::Trace => {
                let v = value.expect("Value flag has value");
                state.trace = if v == "-" || v == "%" {
                    None
                } else {
                    Some(PathBuf::from(v))
                };
            }
            FlagId::TraceAscii => {
                let v = value.expect("Value flag has value");
                state.trace_ascii = if v == "-" || v == "%" {
                    None
                } else {
                    Some(PathBuf::from(v))
                };
            }
            FlagId::EtagSave => {
                state.etag_save = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::EtagCompare => {
                state.etag_compare = Some(PathBuf::from(value.expect("Value flag has value")));
            }
            FlagId::WriteOut => {
                let v = value.expect("Value flag has value");
                if let Some(rest) = v.strip_prefix('@') {
                    if rest == "-" {
                        return Err(ParseError::StreamingUnsupported);
                    }
                    state.write_out_file = Some(PathBuf::from(rest));
                } else {
                    state.write_out_file = None;
                }
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

/// Split a `-b` / `--cookie` value into its `Inline` / `File` form.
///
/// Curl treats a leading `@` as "load this Netscape cookie file". A
/// bare `-` after the `@` would mean "load from stdin" — we never
/// stream stdin bytes through the parser (see `build_body` for the
/// matching `-d @-` rejection), so this fails closed with
/// `ParseError::StreamingUnsupported`.
fn classify_cookie(raw: &str) -> Result<CookieChunk, ParseError> {
    if let Some(rest) = raw.strip_prefix('@') {
        if rest == "-" {
            return Err(ParseError::StreamingUnsupported);
        }
        return Ok(CookieChunk::File(PathBuf::from(rest)));
    }
    Ok(CookieChunk::Inline(raw.to_string()))
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
    cwd: Option<&Path>,
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

    let (body, file_read_effect) = build_body(&state, stdin, cwd)?;

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
            path: resolve_path(upload_path, cwd),
        }));
    }
    if let Some(cert_path) = &state.cert {
        effects.push(Effect::FileRead(FileRead {
            path: resolve_path(cert_path, cwd),
        }));
    }
    if let Some(key_path) = &state.key {
        effects.push(Effect::FileRead(FileRead {
            path: resolve_path(key_path, cwd),
        }));
    }
    if let Some(pubkey_path) = &state.pubkey {
        effects.push(Effect::FileRead(FileRead {
            path: resolve_path(pubkey_path, cwd),
        }));
    }
    for c in &state.cookies {
        if let CookieChunk::File(path) = c {
            effects.push(Effect::FileRead(FileRead {
                path: resolve_path(path, cwd),
            }));
        }
    }
    if let Some(jar) = &state.cookie_jar {
        effects.push(Effect::FileWrite(FileWrite {
            path: resolve_path(jar, cwd),
            source: WriteSource::RemoteHttp { url: url.clone() },
            overwrite: true,
        }));
    }
    // Diagnostic outputs: `-D`, `--trace`, `--trace-ascii`,
    // `--etag-save`. All four write data derived from the HTTP
    // transaction, so they share `WriteSource::RemoteHttp { url }`
    // with `--cookie-jar` / `-o`. The state fields are already
    // `None` for the `-` (stdout) / `%` (stderr) sentinels — see
    // `absorb`.
    for diag_path in [
        state.dump_header.as_ref(),
        state.trace.as_ref(),
        state.trace_ascii.as_ref(),
        state.etag_save.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        effects.push(Effect::FileWrite(FileWrite {
            path: resolve_path(diag_path, cwd),
            source: WriteSource::RemoteHttp { url: url.clone() },
            overwrite: true,
        }));
    }
    if let Some(etag) = &state.etag_compare {
        effects.push(Effect::FileRead(FileRead {
            path: resolve_path(etag, cwd),
        }));
    }
    if let Some(fmt) = &state.write_out_file {
        effects.push(Effect::FileRead(FileRead {
            path: resolve_path(fmt, cwd),
        }));
    }
    if let Some(write) = build_file_write(&state, &url, cwd) {
        effects.push(Effect::FileWrite(write));
    }

    let signals = build_signals(&state);

    let display_hints = build_display_hints(&method, &url, &state);

    let extras = build_extras(&state);

    Ok(ParsedCommand {
        command: "curl".into(),
        argv: argv.to_vec(),
        cwd: cwd.map(Path::to_path_buf),
        effects,
        signals,
        display_hints,
        extras,
    })
}

/// Resolve `path` against `cwd` so the result is suitable to feed into
/// [`crate::signals::analyze`]'s `path_is_inside` check (which compares
/// absolute paths logically).
///
/// - Absolute `path` is returned unchanged.
/// - When `cwd` is `None`, `path` is returned unchanged. This matches
///   the corpus-test driver, which builds an empty `EnvSnapshot`.
/// - Otherwise the path is joined onto `cwd` and any `.` / `..`
///   segments are collapsed without touching the filesystem (so this
///   stays a pure parser-side transform; no symlink semantics yet —
///   the post-launch backlog tracks that).
fn resolve_path(path: &Path, cwd: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let Some(cwd) = cwd else {
        return path.to_path_buf();
    };
    let joined = cwd.join(path);
    let mut out = PathBuf::new();
    for comp in joined.components() {
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
    _stdin: &StdinHandle<'_>,
    cwd: Option<&Path>,
) -> Result<(Body, Option<FileRead>), ParseError> {
    if state.data.is_empty() {
        return Ok((Body::None, None));
    }

    // `-d @-` (and its `--data-binary @-` / `--data-raw @-` /
    // `--data-ascii @-` / `--data-urlencode @-` aliases) sources the
    // request body from stdin. The daemon re-parses argv with an
    // empty stdin (see `vetterd::parse_request`), so any bytes the
    // agent pipes never reach the approver — the popover and audit
    // log would record an empty body while `vet` execs the real
    // pipe. Rather than ship that drift we fail closed: agents must
    // materialise the bytes to a temp file and use `-d @file`, which
    // round-trips through `Body::FromFile` + `FileRead` and stays
    // auditable end-to-end. Closes ThreatModel T9.
    if state.data.iter().any(|c| matches!(c, DataChunk::Stdin)) {
        return Err(ParseError::StreamingUnsupported);
    }

    // Single file chunk → FromFile + FileRead. Resolve once and reuse
    // so Body::FromFile.path and the FileRead.path stay in lockstep.
    if state.data.len() == 1 {
        if let DataChunk::File(path) = &state.data[0] {
            let resolved = resolve_path(path, cwd);
            return Ok((
                Body::FromFile {
                    path: resolved.clone(),
                },
                Some(FileRead { path: resolved }),
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
            DataChunk::File(p) => file_reads.push(FileRead {
                path: resolve_path(p, cwd),
            }),
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
    Ok((body, fr))
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

fn build_file_write(state: &CurlState, url: &Url, cwd: Option<&Path>) -> Option<FileWrite> {
    let raw: PathBuf = if let Some(path) = &state.output {
        path.clone()
    } else if state.remote_name || state.remote_header_name {
        let basename = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("download");
        PathBuf::from(basename)
    } else {
        return None;
    };
    // `--output-dir` is curl's documented prefix for relative output
    // paths (including `-O`/`-J` basenames). Absolute `-o /abs/...`
    // ignores it, matching real curl semantics.
    let combined = match &state.output_dir {
        Some(dir) if !raw.is_absolute() => dir.join(&raw),
        _ => raw,
    };
    Some(FileWrite {
        path: resolve_path(&combined, cwd),
        source: WriteSource::RemoteHttp { url: url.clone() },
        overwrite: !state.no_clobber,
    })
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
    if let Some(p) = &state.cert {
        out.push(RiskSignal {
            kind: SignalKind::ClientCertificate,
            detail: format!("--cert {}", p.display()),
            effect_idx: Some(0),
        });
    }
    if let Some(p) = &state.key {
        out.push(RiskSignal {
            kind: SignalKind::ClientCertificate,
            detail: format!("--key {}", p.display()),
            effect_idx: Some(0),
        });
    }
    if state.remote_header_name {
        out.push(RiskSignal {
            kind: SignalKind::RemoteHeaderName,
            detail: "filename from response Content-Disposition (placeholder path)".into(),
            effect_idx: Some(0),
        });
    }
    if state.create_dirs {
        out.push(RiskSignal {
            kind: SignalKind::CreateDirs,
            detail: "--create-dirs: writes may land in newly-created subdirectories".into(),
            effect_idx: Some(0),
        });
    }
    if state.follow_redirects {
        out.push(RiskSignal {
            kind: SignalKind::FollowRedirects,
            detail: "-L follows HTTP redirects".into(),
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
    if let Some(t) = &state.cert_type {
        extras.insert(
            "cert_type".to_string(),
            serde_json::Value::String(t.clone()),
        );
    }
    if let Some(t) = &state.key_type {
        extras.insert("key_type".to_string(), serde_json::Value::String(t.clone()));
    }
    if let Some(e) = &state.engine {
        extras.insert("engine".to_string(), serde_json::Value::String(e.clone()));
    }
    if state.cert_password_supplied {
        extras.insert(
            "cert_password_supplied".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    let inline_cookies: Vec<serde_json::Value> = state
        .cookies
        .iter()
        .filter_map(|c| match c {
            CookieChunk::Inline(s) => Some(serde_json::Value::String(s.clone())),
            CookieChunk::File(_) => None,
        })
        .collect();
    if !inline_cookies.is_empty() {
        extras.insert(
            "cookies_inline".to_string(),
            serde_json::Value::Array(inline_cookies),
        );
    }
    if extras.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(extras)
    }
}

#[cfg(test)]
#[path = "../../tests/parsers_curl_state.rs"]
mod tests;
