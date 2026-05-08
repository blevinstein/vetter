//! Tests for [`crate::parsers::curl::state`]. Layout convention is
//! described in `AGENTS.md`.

use super::*;

fn argv(args: &[&str]) -> Vec<String> {
    std::iter::once("curl".to_string())
        .chain(args.iter().map(|a| a.to_string()))
        .collect()
}

fn parse(args: &[&str]) -> ParsedCommand {
    parse_argv(&argv(args), &StdinHandle::empty(), None).expect("parse")
}

fn parse_with_cwd(args: &[&str], cwd: &str) -> ParsedCommand {
    parse_argv(
        &argv(args),
        &StdinHandle::empty(),
        Some(std::path::Path::new(cwd)),
    )
    .expect("parse")
}

fn parse_stdin(args: &[&str], stdin: &[u8], cap: usize) -> Result<ParsedCommand, ParseError> {
    parse_argv(&argv(args), &StdinHandle::from_bytes(stdin, cap), None)
}

fn http_of(p: &ParsedCommand) -> &HttpRequest {
    match &p.effects[0] {
        Effect::HttpRequest(r) => r,
        _ => panic!("expected http"),
    }
}

#[test]
fn missing_url_errors() {
    let r = parse_argv(&argv(&[]), &StdinHandle::empty(), None);
    assert!(matches!(r, Err(ParseError::MissingArgument(_))));
}

#[test]
fn missing_url_with_flags_only_errors() {
    let r = parse_argv(&argv(&["-k"]), &StdinHandle::empty(), None);
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
        None,
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
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn chunked_transfer_header_rejected_case_insensitive() {
    let r = parse_argv(
        &argv(&["-H", "transfer-encoding: Chunked", "https://example.test/"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::StreamingUnsupported)));
}

#[test]
fn upload_stdin_rejected() {
    let r = parse_argv(
        &argv(&["-T", "-", "https://example.test/"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::StreamingUnsupported)));
}

#[test]
fn config_long_flag_rejected() {
    let r = parse_argv(
        &argv(&["--config", "some/file.cfg", "https://example.test/"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn config_short_flag_rejected() {
    let r = parse_argv(
        &argv(&["-K", "some/file.cfg", "https://example.test/"]),
        &StdinHandle::empty(),
        None,
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
        None,
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
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn next_short_flag_rejected() {
    let r = parse_argv(
        &argv(&["https://safe.example/", "-:", "https://attacker.example/"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn form_long_flag_rejected() {
    // Multipart curl is uncommon in agent workflows and the parser does
    // not yet model it precisely; reject up front rather than mis-vet
    // (mirrors --config / --next).
    let r = parse_argv(
        &argv(&["--form", "name=value", "https://example.test/upload"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn form_short_flag_rejected() {
    // -F upload=@./photo.png would today fall through to extras and
    // ship without a FileRead effect; reject instead.
    let r = parse_argv(
        &argv(&["-F", "upload=@./photo.png", "https://example.test/upload"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn form_string_flag_rejected() {
    // --form-string is the literal-only sibling of -F; we still refuse
    // so the rejection surface is uniform.
    let r = parse_argv(
        &argv(&["--form-string", "note=hello", "https://example.test/notes"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn form_short_in_cluster_rejected() {
    // -kF clusters -k (insecure, Bool) with -F (form, Value); the
    // tokeniser rejects value-taking shorts inside a cluster, so this
    // surfaces as ParseError::Other before the absorb arm runs.
    let r = parse_argv(
        &argv(&["-kF", "upload=@./x", "https://example.test/upload"]),
        &StdinHandle::empty(),
        None,
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
        None,
    );
    assert!(matches!(r, Err(ParseError::Other(_))));
}

#[test]
fn data_at_stdin_rejected_as_streaming_unsupported() {
    // ThreatModel T9 close: `-d @-` would source the body from the
    // client-side pipe, but the daemon re-parses argv with an empty
    // stdin handle (see `vetterd::parse_request`) so the popover and
    // audit log would record a 0-byte body while `vet` execs the real
    // pipe. Fail closed in the parser; agents must materialise the
    // bytes to a temp file and use `-d @file` instead. The rejection
    // is independent of stdin contents (stdin is intentionally
    // ignored), so empty / small / oversize all surface the same
    // `StreamingUnsupported` error.
    for stdin in [&b""[..], &b"hello"[..], &[b'a'; 4096][..]] {
        let r = parse_stdin(&["-d", "@-", "https://example.test/"], stdin, 1024);
        assert!(
            matches!(r, Err(ParseError::StreamingUnsupported)),
            "stdin len {} should be rejected, got {r:?}",
            stdin.len()
        );
    }
}

#[test]
fn data_alias_at_stdin_rejected() {
    // The `@-` rejection must apply to every `-d` alias that honours
    // the `@` prefix: `--data` (long form), `--data-binary`,
    // `--data-ascii`, `--data-urlencode`. All four funnel through
    // `state.data` as a `DataChunk::Stdin`, so one `build_body`
    // branch handles them — these assertions pin the rejection
    // across the alias surface so a future flag-table edit can't
    // silently re-enable the gap. `--data-raw` is intentionally
    // excluded: per curl, it never interprets `@`, so `@-` is the
    // literal string `@-`. See `data_raw_at_dash_is_inline_literal`.
    for flag in [
        "--data",
        "--data-binary",
        "--data-ascii",
        "--data-urlencode",
    ] {
        let r = parse_stdin(&[flag, "@-", "https://example.test/"], b"hi", 1024);
        assert!(
            matches!(r, Err(ParseError::StreamingUnsupported)),
            "{flag} @- should be rejected, got {r:?}",
        );
    }
}

#[test]
fn data_raw_at_dash_is_inline_literal() {
    // `--data-raw` deliberately ignores the `@` prefix per curl, so
    // `--data-raw @-` is a two-byte inline body, not a stdin
    // reference — and stays outside the `data_at_stdin_*` rejection
    // surface above.
    let p = parse_stdin(
        &["--data-raw", "@-", "https://example.test/"],
        b"ignored",
        1024,
    )
    .expect("parse");
    match &http_of(&p).body {
        Body::Inline { bytes } => assert_eq!(bytes, b"@-"),
        other => panic!("expected Body::Inline(@-), got {other:?}"),
    }
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

// -- relative-path resolution against EnvSnapshot::cwd ---------------
//
// Closes the Phase 5.2 "spurious FileOutsideCwd" item: the parser must
// resolve relative `FileRead` / `FileWrite` paths against the agent's
// cwd before constructing effects, so downstream `signals::analyze`
// (which compares absolute paths via `path_is_inside`) stops firing
// false positives for things like `-o ./out` or `-O thing.tgz`.

fn first_file_write(p: &ParsedCommand) -> &FileWrite {
    p.effects
        .iter()
        .find_map(|e| {
            if let Effect::FileWrite(w) = e {
                Some(w)
            } else {
                None
            }
        })
        .expect("expected a file_write effect")
}

fn first_file_read(p: &ParsedCommand) -> &FileRead {
    p.effects
        .iter()
        .find_map(|e| {
            if let Effect::FileRead(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .expect("expected a file_read effect")
}

#[test]
fn output_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["-o", "./out", "https://example.test/x"], "/work");
    assert_eq!(first_file_write(&p).path, PathBuf::from("/work/out"));
}

#[test]
fn output_bare_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["-o", "out.json", "https://example.test/x"], "/work");
    assert_eq!(first_file_write(&p).path, PathBuf::from("/work/out.json"));
}

#[test]
fn output_absolute_path_unchanged_when_cwd_set() {
    let p = parse_with_cwd(&["-o", "/tmp/out", "https://example.test/x"], "/work");
    assert_eq!(first_file_write(&p).path, PathBuf::from("/tmp/out"));
}

#[test]
fn output_relative_with_parent_collapses() {
    let p = parse_with_cwd(
        &["-o", "../sibling/x", "https://example.test/y"],
        "/work/sub",
    );
    assert_eq!(first_file_write(&p).path, PathBuf::from("/work/sibling/x"));
}

#[test]
fn remote_name_basename_resolved_against_cwd() {
    let p = parse_with_cwd(&["-O", "https://example.test/dir/thing.tgz"], "/work");
    assert_eq!(first_file_write(&p).path, PathBuf::from("/work/thing.tgz"));
}

#[test]
fn upload_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["-T", "rel", "https://example.test/x"], "/work");
    assert_eq!(first_file_read(&p).path, PathBuf::from("/work/rel"));
}

#[test]
fn upload_absolute_path_unchanged_when_cwd_set() {
    let p = parse_with_cwd(&["-T", "/tmp/x", "https://example.test/x"], "/work");
    assert_eq!(first_file_read(&p).path, PathBuf::from("/tmp/x"));
}

#[test]
fn data_at_file_relative_path_resolved_and_body_matches() {
    let p = parse_with_cwd(
        &["-d", "@./payload.json", "https://example.test/submit"],
        "/work",
    );
    let resolved = PathBuf::from("/work/payload.json");
    assert_eq!(first_file_read(&p).path, resolved);
    match &http_of(&p).body {
        Body::FromFile { path } => assert_eq!(path, &resolved),
        other => panic!("expected FromFile, got {other:?}"),
    }
}

#[test]
fn parsed_cwd_is_threaded_from_env() {
    let p = parse_with_cwd(&["https://example.test/"], "/work");
    assert_eq!(p.cwd.as_deref(), Some(std::path::Path::new("/work")));
}

#[test]
fn relative_paths_unchanged_when_cwd_absent() {
    // Regression guard: the corpus driver and the signal-free unit
    // tests above all parse with `cwd = None`. In that mode the parser
    // must leave file paths verbatim so existing snapshots stay
    // byte-identical.
    let p = parse(&["-d", "@./payload.json", "https://example.test/submit"]);
    assert_eq!(first_file_read(&p).path, PathBuf::from("./payload.json"));
    assert!(p.cwd.is_none());
}

#[test]
fn relative_output_does_not_trigger_file_outside_cwd_signal() {
    // The bug fix: with cwd=/work, `-o out` resolves to `/work/out`,
    // which `path_is_inside` recognises as inside cwd. The generic
    // analyzer should produce zero `FileOutsideCwd` signals.
    let p = parse_with_cwd(&["-o", "out", "https://example.test/x"], "/work");
    let signals = crate::signals::analyze(&p);
    assert!(
        !signals.iter().any(|s| s.kind == SignalKind::FileOutsideCwd),
        "unexpected FileOutsideCwd signal: {signals:?}"
    );
}

#[test]
fn absolute_output_outside_cwd_still_triggers_file_outside_cwd_signal() {
    // Regression guard for the inverse: a real out-of-cwd write must
    // still surface the signal so the user sees the warning.
    let p = parse_with_cwd(&["-o", "/etc/passwd", "https://example.test/x"], "/work");
    let signals = crate::signals::analyze(&p);
    assert!(
        signals.iter().any(|s| s.kind == SignalKind::FileOutsideCwd),
        "expected FileOutsideCwd signal, got: {signals:?}"
    );
}

#[test]
fn relative_upload_does_not_trigger_file_read_outside_cwd_signal() {
    let p = parse_with_cwd(&["-T", "payload.bin", "https://example.test/x"], "/work");
    let signals = crate::signals::analyze(&p);
    assert!(
        !signals
            .iter()
            .any(|s| s.kind == SignalKind::FileReadOutsideCwd),
        "unexpected FileReadOutsideCwd signal: {signals:?}"
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
            | FlagId::OutputDir
            | FlagId::NoClobber
            | FlagId::CreateDirs
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
            | FlagId::Next
            | FlagId::Form
            | FlagId::FormString
            | FlagId::Cert
            | FlagId::Key
            | FlagId::CertType
            | FlagId::KeyType
            | FlagId::Pass
            | FlagId::Pubkey
            | FlagId::Engine
            | FlagId::Cookie
            | FlagId::CookieJar
            | FlagId::DumpHeader
            | FlagId::Trace
            | FlagId::TraceAscii
            | FlagId::EtagSave
            | FlagId::EtagCompare
            | FlagId::WriteOut => {}
        }
    }
    let _ = _check;
}

// -- client-TLS material flags ---------------------------------------
//
// Closes the Phase 5.2 "cert/key/pubkey FileRead + ClientCertificate
// signal" item. `--cert`, `--key`, and `--pubkey` are paths and must
// surface as `FileRead` effects. `--cert` and `--key` additionally
// push a `ClientCertificate` signal (Danger). `--cert-type`,
// `--key-type`, `--engine`, and `--pass` are non-path strings and
// must not produce file effects; `--pass` must never have its value
// echoed anywhere in the parsed command.

fn collect_file_reads(p: &ParsedCommand) -> Vec<&FileRead> {
    p.effects
        .iter()
        .filter_map(|e| {
            if let Effect::FileRead(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .collect()
}

fn parsed_command_contains_substring(p: &ParsedCommand, needle: &str) -> bool {
    if p.signals.iter().any(|s| s.detail.contains(needle)) {
        return true;
    }
    if p.display_hints.primary_target.contains(needle)
        || p.display_hints.primary_verb.contains(needle)
        || p.display_hints
            .badges
            .iter()
            .any(|b| b.label.contains(needle))
    {
        return true;
    }
    p.extras.to_string().contains(needle)
}

#[test]
fn cert_flag_emits_file_read_and_signal() {
    let p = parse(&["--cert", "/tmp/c.pem", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/c.pem"));
    assert_eq!(
        p.signals
            .iter()
            .filter(|s| s.kind == SignalKind::ClientCertificate)
            .count(),
        1
    );
}

#[test]
fn cert_with_password_strips_suffix_and_does_not_leak_value() {
    let p = parse(&["--cert", "/tmp/c.pem:hunter2", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/c.pem"));
    assert!(
        !parsed_command_contains_substring(&p, "hunter2"),
        "--cert password leaked into parsed command surface: {p:#?}"
    );
    assert_eq!(
        p.extras
            .get("cert_password_supplied")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn key_flag_emits_file_read_and_signal() {
    let p = parse(&["--key", "/tmp/c.key", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/c.key"));
    assert_eq!(
        p.signals
            .iter()
            .filter(|s| s.kind == SignalKind::ClientCertificate)
            .count(),
        1
    );
}

#[test]
fn pubkey_flag_emits_file_read_no_signal() {
    let p = parse(&["--pubkey", "/tmp/id_rsa.pub", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/id_rsa.pub"));
    assert!(!p
        .signals
        .iter()
        .any(|s| s.kind == SignalKind::ClientCertificate));
}

#[test]
fn cert_type_does_not_emit_file_read() {
    let p = parse(&["--cert-type", "PEM", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert!(
        reads.is_empty(),
        "--cert-type PEM produced unexpected FileRead effects: {reads:?}"
    );
    assert_eq!(
        p.extras.get("cert_type").and_then(|v| v.as_str()),
        Some("PEM")
    );
}

#[test]
fn key_type_and_engine_surface_in_extras() {
    let p = parse(&[
        "--key-type",
        "DER",
        "--engine",
        "dynamic",
        "https://example.test/",
    ]);
    assert!(collect_file_reads(&p).is_empty());
    assert_eq!(
        p.extras.get("key_type").and_then(|v| v.as_str()),
        Some("DER")
    );
    assert_eq!(
        p.extras.get("engine").and_then(|v| v.as_str()),
        Some("dynamic")
    );
}

#[test]
fn pass_flag_value_not_leaked() {
    let p = parse(&["--pass", "hunter2", "https://example.test/"]);
    assert!(
        !parsed_command_contains_substring(&p, "hunter2"),
        "--pass value leaked into parsed command surface: {p:#?}"
    );
    assert_eq!(
        p.extras
            .get("cert_password_supplied")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(collect_file_reads(&p).is_empty());
}

#[test]
fn cert_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["--cert", "client.pem", "https://example.test/"], "/work");
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/work/client.pem"));
}

#[test]
fn cert_short_e_alias_works() {
    // `-E` is the curl-canonical short form of `--cert`. Make sure the
    // tokeniser routes it the same way as the long form.
    let p = parse(&["-E", "/tmp/c.pem", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/c.pem"));
    assert!(p
        .signals
        .iter()
        .any(|s| s.kind == SignalKind::ClientCertificate));
}

// -- cookie I/O ------------------------------------------------------
//
// Closes the Phase 5.2 "cookie file FileRead / cookie-jar FileWrite"
// items. `-b @file` loads cookies from a Netscape-format file (a
// credential file) and must surface as a FileRead; `-b "k=v"` is
// purely header-shaped and only lands in `extras.cookies_inline`.
// `-c <file>` writes the in-memory cookie jar on exit and surfaces
// as a FileWrite with `WriteSource::RemoteHttp`.

fn collect_file_writes(p: &ParsedCommand) -> Vec<&FileWrite> {
    p.effects
        .iter()
        .filter_map(|e| {
            if let Effect::FileWrite(w) = e {
                Some(w)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn cookie_file_emits_file_read() {
    let p = parse(&["-b", "@/tmp/cookies.txt", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/cookies.txt"));
    assert!(
        p.extras.get("cookies_inline").is_none(),
        "file-form cookie should not populate cookies_inline: {:?}",
        p.extras
    );
}

#[test]
fn cookie_file_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["-b", "@cookies.txt", "https://example.test/"], "/work");
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/work/cookies.txt"));
}

#[test]
fn cookie_long_alias_works() {
    let p = parse(&["--cookie", "@/tmp/c.txt", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/c.txt"));
}

#[test]
fn cookie_inline_does_not_emit_file_read() {
    let p = parse(&["-b", "session=abc", "https://example.test/"]);
    assert!(
        collect_file_reads(&p).is_empty(),
        "inline cookie produced unexpected FileRead effects"
    );
    assert_eq!(
        p.extras
            .get("cookies_inline")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()),
        Some(vec!["session=abc"])
    );
}

#[test]
fn multiple_cookie_files_emit_one_file_read_each() {
    let p = parse(&[
        "-b",
        "@/tmp/a.txt",
        "-b",
        "@/tmp/b.txt",
        "https://example.test/",
    ]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/a.txt"));
    assert_eq!(reads[1].path, PathBuf::from("/tmp/b.txt"));
}

#[test]
fn mixed_cookie_inline_and_file_split_correctly() {
    let p = parse(&[
        "-b",
        "session=abc",
        "-b",
        "@/tmp/cookies.txt",
        "https://example.test/",
    ]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/cookies.txt"));
    assert_eq!(
        p.extras
            .get("cookies_inline")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()),
        Some(vec!["session=abc"])
    );
}

#[test]
fn cookie_at_dash_rejected_as_streaming_unsupported() {
    let r = parse_argv(
        &argv(&["-b", "@-", "https://example.test/"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::StreamingUnsupported)));
}

#[test]
fn cookie_jar_emits_file_write_with_remote_http_source() {
    let p = parse(&["-c", "/tmp/jar.txt", "https://example.test/login"]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/jar.txt"));
    match &writes[0].source {
        WriteSource::RemoteHttp { url } => {
            assert_eq!(url.as_str(), "https://example.test/login");
        }
        other => panic!("expected RemoteHttp source, got {other:?}"),
    }
    assert!(writes[0].overwrite);
}

#[test]
fn cookie_jar_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["-c", "jar.txt", "https://example.test/"], "/work");
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/work/jar.txt"));
}

#[test]
fn cookie_jar_long_alias_works() {
    let p = parse(&["--cookie-jar", "/tmp/jar.txt", "https://example.test/"]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/jar.txt"));
}

#[test]
fn cookie_jar_last_occurrence_wins() {
    let p = parse(&[
        "-c",
        "/tmp/old.txt",
        "-c",
        "/tmp/new.txt",
        "https://example.test/",
    ]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/new.txt"));
}

#[test]
fn cookie_jar_coexists_with_output_flag() {
    // -c writes the jar; -o writes the response body. Both must
    // surface as separate FileWrite effects.
    let p = parse(&[
        "-c",
        "/tmp/jar.txt",
        "-o",
        "/tmp/body.json",
        "https://example.test/",
    ]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 2);
    let paths: Vec<&std::path::Path> = writes.iter().map(|w| w.path.as_path()).collect();
    assert!(paths.contains(&std::path::Path::new("/tmp/jar.txt")));
    assert!(paths.contains(&std::path::Path::new("/tmp/body.json")));
}

// -- diagnostic outputs ---------------------------------------------
//
// `-D`, `--trace`, `--trace-ascii`, `--etag-save` write data derived
// from the HTTP transaction to disk and surface as `FileWrite` with
// `WriteSource::RemoteHttp { url }` (mirroring `--cookie-jar`).
// `--etag-compare` and `-w @file` surface as `FileRead`. `-w` without
// a leading `@` is informational and emits no effect; `-w @-` is
// rejected like other stdin streams.

#[test]
fn dump_header_emits_file_write_with_remote_http_source() {
    let p = parse(&["-D", "/tmp/headers.txt", "https://example.test/"]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/headers.txt"));
    match &writes[0].source {
        WriteSource::RemoteHttp { url } => {
            assert_eq!(url.as_str(), "https://example.test/");
        }
        other => panic!("expected RemoteHttp source, got {other:?}"),
    }
    assert!(writes[0].overwrite);
}

#[test]
fn dump_header_long_alias_works() {
    let p = parse(&["--dump-header", "/tmp/h.txt", "https://example.test/"]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/h.txt"));
}

#[test]
fn dump_header_dash_emits_no_file_write() {
    // `-D -` dumps to stdout — not a file the user is asking us to
    // approve.
    let p = parse(&["-D", "-", "https://example.test/"]);
    assert!(
        collect_file_writes(&p).is_empty(),
        "`-D -` produced unexpected FileWrite effects"
    );
}

#[test]
fn dump_header_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["-D", "headers.txt", "https://example.test/"], "/work");
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/work/headers.txt"));
}

#[test]
fn dump_header_last_occurrence_wins() {
    let p = parse(&[
        "-D",
        "/tmp/old.txt",
        "-D",
        "/tmp/new.txt",
        "https://example.test/",
    ]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/new.txt"));
}

#[test]
fn trace_emits_file_write() {
    let p = parse(&["--trace", "/tmp/trace.bin", "https://example.test/"]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/trace.bin"));
    match &writes[0].source {
        WriteSource::RemoteHttp { url } => {
            assert_eq!(url.as_str(), "https://example.test/");
        }
        other => panic!("expected RemoteHttp source, got {other:?}"),
    }
}

#[test]
fn trace_dash_no_effect() {
    let p = parse(&["--trace", "-", "https://example.test/"]);
    assert!(
        collect_file_writes(&p).is_empty(),
        "`--trace -` produced unexpected FileWrite effects"
    );
}

#[test]
fn trace_percent_no_effect() {
    let p = parse(&["--trace", "%", "https://example.test/"]);
    assert!(
        collect_file_writes(&p).is_empty(),
        "`--trace %` produced unexpected FileWrite effects"
    );
}

#[test]
fn trace_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["--trace", "trace.bin", "https://example.test/"], "/work");
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/work/trace.bin"));
}

#[test]
fn trace_ascii_emits_file_write() {
    let p = parse(&["--trace-ascii", "/tmp/trace.txt", "https://example.test/"]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/trace.txt"));
}

#[test]
fn trace_ascii_dash_no_effect() {
    let p = parse(&["--trace-ascii", "-", "https://example.test/"]);
    assert!(collect_file_writes(&p).is_empty());
}

#[test]
fn trace_ascii_percent_no_effect() {
    let p = parse(&["--trace-ascii", "%", "https://example.test/"]);
    assert!(collect_file_writes(&p).is_empty());
}

#[test]
fn etag_save_emits_file_write_with_remote_http_source() {
    let p = parse(&["--etag-save", "/tmp/etag", "https://example.test/data.json"]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/tmp/etag"));
    match &writes[0].source {
        WriteSource::RemoteHttp { url } => {
            assert_eq!(url.as_str(), "https://example.test/data.json");
        }
        other => panic!("expected RemoteHttp source, got {other:?}"),
    }
}

#[test]
fn etag_save_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["--etag-save", "etag", "https://example.test/"], "/work");
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, PathBuf::from("/work/etag"));
}

#[test]
fn etag_compare_emits_file_read() {
    let p = parse(&[
        "--etag-compare",
        "/tmp/etag",
        "https://example.test/data.json",
    ]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/etag"));
    assert!(
        collect_file_writes(&p).is_empty(),
        "`--etag-compare` should not produce a FileWrite"
    );
}

#[test]
fn etag_compare_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(
        &["--etag-compare", "etag", "https://example.test/"],
        "/work",
    );
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/work/etag"));
}

#[test]
fn etag_save_and_compare_coexist_as_separate_effects() {
    let p = parse(&[
        "--etag-save",
        "/tmp/etag",
        "--etag-compare",
        "/tmp/etag",
        "https://example.test/data.json",
    ]);
    assert_eq!(collect_file_writes(&p).len(), 1);
    assert_eq!(collect_file_reads(&p).len(), 1);
}

#[test]
fn write_out_at_file_emits_file_read() {
    let p = parse(&["-w", "@/tmp/fmt.txt", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/fmt.txt"));
    assert!(
        collect_file_writes(&p).is_empty(),
        "`-w @file` should not produce a FileWrite"
    );
}

#[test]
fn write_out_long_alias_at_file_emits_file_read() {
    let p = parse(&["--write-out", "@/tmp/fmt.txt", "https://example.test/"]);
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/tmp/fmt.txt"));
}

#[test]
fn write_out_inline_format_emits_no_effect() {
    let p = parse(&["-w", "%{http_code}\n", "https://example.test/"]);
    assert!(collect_file_reads(&p).is_empty());
    assert!(collect_file_writes(&p).is_empty());
}

#[test]
fn write_out_at_dash_rejected() {
    let r = parse_argv(
        &argv(&["-w", "@-", "https://example.test/"]),
        &StdinHandle::empty(),
        None,
    );
    assert!(matches!(r, Err(ParseError::StreamingUnsupported)), "{r:?}");
}

#[test]
fn write_out_relative_path_resolved_against_cwd() {
    let p = parse_with_cwd(&["-w", "@fmt.txt", "https://example.test/"], "/work");
    let reads = collect_file_reads(&p);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].path, PathBuf::from("/work/fmt.txt"));
}

#[test]
fn dump_header_etag_save_and_output_emit_three_writes() {
    // Mirrors `cookie_jar_coexists_with_output_flag`: three flags
    // each contribute their own FileWrite effect.
    let p = parse(&[
        "-D",
        "/tmp/headers.txt",
        "--etag-save",
        "/tmp/etag",
        "-o",
        "/tmp/body.json",
        "https://example.test/data.json",
    ]);
    let writes = collect_file_writes(&p);
    assert_eq!(writes.len(), 3);
    let paths: Vec<&std::path::Path> = writes.iter().map(|w| w.path.as_path()).collect();
    assert!(paths.contains(&std::path::Path::new("/tmp/headers.txt")));
    assert!(paths.contains(&std::path::Path::new("/tmp/etag")));
    assert!(paths.contains(&std::path::Path::new("/tmp/body.json")));
}

// -- --output-dir / --no-clobber / --create-dirs / -J coverage ------
//
// Closes four Phase 5.2 boxes that converge on `build_file_write`:
// the FileWrite the parser emits for `-o` / `-O` / `-J`.

#[test]
fn output_dir_prefix_applies_to_dash_o() {
    let p = parse(&[
        "--output-dir",
        "downloads",
        "-o",
        "report.json",
        "https://example.test/x",
    ]);
    assert_eq!(
        first_file_write(&p).path,
        PathBuf::from("downloads/report.json")
    );
}

#[test]
fn output_dir_prefix_ignored_for_absolute_dash_o() {
    let p = parse(&[
        "--output-dir",
        "downloads",
        "-o",
        "/tmp/report.json",
        "https://example.test/x",
    ]);
    assert_eq!(first_file_write(&p).path, PathBuf::from("/tmp/report.json"));
}

#[test]
fn output_dir_prefix_applies_to_dash_o_basename() {
    let p = parse(&[
        "--output-dir",
        "downloads",
        "-O",
        "https://example.test/dir/thing.tgz",
    ]);
    assert_eq!(
        first_file_write(&p).path,
        PathBuf::from("downloads/thing.tgz")
    );
}

#[test]
fn output_dir_resolves_with_cwd_when_dir_is_relative() {
    let p = parse_with_cwd(
        &[
            "--output-dir",
            "downloads",
            "-o",
            "report.json",
            "https://example.test/x",
        ],
        "/work",
    );
    assert_eq!(
        first_file_write(&p).path,
        PathBuf::from("/work/downloads/report.json")
    );
}

#[test]
fn no_clobber_flips_overwrite_false() {
    let p = parse(&["--no-clobber", "-o", "/tmp/x", "https://example.test/y"]);
    assert!(!first_file_write(&p).overwrite);
}

#[test]
fn default_overwrite_remains_true_when_no_clobber_absent() {
    let p = parse(&["-o", "/tmp/x", "https://example.test/y"]);
    assert!(first_file_write(&p).overwrite);
}

#[test]
fn remote_header_name_pushes_signal() {
    let p = parse(&["-J", "-O", "https://example.test/dir/thing.tgz"]);
    assert!(p
        .signals
        .iter()
        .any(|s| s.kind == SignalKind::RemoteHeaderName));
}

#[test]
fn remote_header_name_signal_absent_without_dash_j() {
    let p = parse(&["-O", "https://example.test/dir/thing.tgz"]);
    assert!(!p
        .signals
        .iter()
        .any(|s| s.kind == SignalKind::RemoteHeaderName));
}

#[test]
fn create_dirs_pushes_signal() {
    let p = parse(&[
        "--create-dirs",
        "-o",
        "/tmp/sub/dir/x",
        "https://example.test/y",
    ]);
    assert!(p.signals.iter().any(|s| s.kind == SignalKind::CreateDirs));
}

#[test]
fn create_dirs_signal_absent_without_flag() {
    let p = parse(&["-o", "/tmp/sub/dir/x", "https://example.test/y"]);
    assert!(!p.signals.iter().any(|s| s.kind == SignalKind::CreateDirs));
}

// -- -L / --location follow-redirects -------------------------------
//
// Closes the H2 / ThreatModel T10 item: `-L` flips
// `HttpRequest.follow_redirects` and pushes a `FollowRedirects` signal
// so the matcher's `no_redirects` predicate (default-deny) can refuse
// to auto-allow rules that don't explicitly opt into redirect-
// following trust.

#[test]
fn location_short_flag_pushes_follow_redirects_signal() {
    let p = parse(&["-L", "https://example.test/redir"]);
    assert!(http_of(&p).follow_redirects);
    assert_eq!(
        p.signals
            .iter()
            .filter(|s| s.kind == SignalKind::FollowRedirects)
            .count(),
        1
    );
}

#[test]
fn location_long_flag_pushes_follow_redirects_signal() {
    let p = parse(&["--location", "https://example.test/redir"]);
    assert!(http_of(&p).follow_redirects);
    assert!(p
        .signals
        .iter()
        .any(|s| s.kind == SignalKind::FollowRedirects));
}

#[test]
fn follow_redirects_signal_absent_without_flag() {
    let p = parse(&["https://example.test/"]);
    assert!(!http_of(&p).follow_redirects);
    assert!(!p
        .signals
        .iter()
        .any(|s| s.kind == SignalKind::FollowRedirects));
}
