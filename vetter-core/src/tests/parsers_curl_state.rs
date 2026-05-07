//! Tests for [`crate::parsers::curl::state`]. Layout convention is
//! described in `AGENTS.md`.

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
fn config_long_flag_rejected() {
    let r = parse_argv(
        &argv(&["--config", "some/file.cfg", "https://example.test/"]),
        &StdinHandle::empty(),
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn config_short_flag_rejected() {
    let r = parse_argv(
        &argv(&["-K", "some/file.cfg", "https://example.test/"]),
        &StdinHandle::empty(),
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn config_short_flag_from_stdin_rejected() {
    // `-K -` reads the curl config from stdin; we still refuse so an
    // attacker cannot pipe a config that overrides every other flag.
    let r = parse_argv(
        &argv(&["-K", "-", "https://example.test/"]),
        &StdinHandle::empty(),
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn next_long_flag_rejected() {
    // `--next` chains a second curl invocation in the same argv; we
    // refuse rather than render only the first request.
    let r = parse_argv(
        &argv(&[
            "https://safe.example/",
            "--next",
            "https://attacker.example/",
        ]),
        &StdinHandle::empty(),
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn next_short_flag_rejected() {
    let r = parse_argv(
        &argv(&["https://safe.example/", "-:", "https://attacker.example/"]),
        &StdinHandle::empty(),
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn next_short_in_cluster_rejected() {
    // `-k:` clusters `-k` (insecure, Bool) with `-:` (next, Bool); the
    // tokeniser must surface the `:` so finalisation can reject it.
    let r = parse_argv(
        &argv(&["-k:", "https://safe.example/", "https://attacker.example/"]),
        &StdinHandle::empty(),
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
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
            | FlagId::ProgressBar
            | FlagId::Config
            | FlagId::Next => {}
        }
    }
    let _ = _check;
}
