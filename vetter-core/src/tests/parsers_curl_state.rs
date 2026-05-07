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
            | FlagId::Engine => {}
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
