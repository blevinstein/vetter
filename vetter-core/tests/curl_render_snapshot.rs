//! End-to-end render snapshots for the curl parser.
//!
//! Picks three representative fixtures from `tests/corpus/curl/`,
//! parses them, runs the generic risk analyzer, and feeds the result
//! through `DefaultRenderer` with both `PlainWriter` and `AnsiWriter`.
//! This exercises the full Phase 1a pipeline against the new Phase 1b
//! parser without a daemon or policy layer.
//!
//! Snapshots are stored under `tests/snapshots/` so failures show a
//! readable diff. Update with `INSTA_UPDATE=always cargo test
//! --workspace --all-features`.

use std::fs;
use std::path::{Path, PathBuf};

use vetter_core::{
    analyze,
    parsers::{curl::CurlParser, CommandParser, EnvSnapshot, StdinHandle},
    AnsiWriter, DefaultRenderer, PlainWriter, Renderer,
};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/curl")
}

fn read_argv(name: &str) -> Vec<String> {
    let path = corpus_dir().join(format!("{name}.argv"));
    let body = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = vec!["curl".to_string()];
    for raw in body.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        out.push(line.to_string());
    }
    out
}

fn parsed_fixture(name: &str) -> vetter_core::ParsedCommand {
    let parser = CurlParser;
    let mut p = parser
        .parse(
            &read_argv(name),
            StdinHandle::empty(),
            &EnvSnapshot::default(),
        )
        .unwrap_or_else(|e| panic!("fixture {name} parse: {e}"));
    // Extend the parser's pushed signals with the generic analyzer's,
    // matching what `vet` will do at runtime.
    let generic = analyze(&p);
    p.signals.extend(generic);
    p
}

fn render_plain(p: &vetter_core::ParsedCommand) -> String {
    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(p, &mut PlainWriter(&mut buf))
        .expect("render plain");
    String::from_utf8(buf).expect("utf-8")
}

fn render_ansi(p: &vetter_core::ParsedCommand) -> String {
    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(p, &mut AnsiWriter(&mut buf))
        .expect("render ansi");
    // Replace ESC with `\e` so snapshot diffs stay readable.
    String::from_utf8(buf)
        .expect("utf-8")
        .replace('\x1b', "\\e")
}

#[test]
fn render_post_json_data_plain() {
    let p = parsed_fixture("post_json_data");
    insta::assert_snapshot!("post_json_data_plain", render_plain(&p));
}

#[test]
fn render_post_json_data_ansi() {
    let p = parsed_fixture("post_json_data");
    insta::assert_snapshot!("post_json_data_ansi", render_ansi(&p));
}

#[test]
fn render_insecure_plain() {
    let p = parsed_fixture("insecure");
    insta::assert_snapshot!("insecure_plain", render_plain(&p));
}

#[test]
fn render_insecure_ansi() {
    let p = parsed_fixture("insecure");
    insta::assert_snapshot!("insecure_ansi", render_ansi(&p));
}

#[test]
fn render_output_file_plain() {
    let p = parsed_fixture("output_file");
    insta::assert_snapshot!("output_file_plain", render_plain(&p));
}

#[test]
fn render_output_file_ansi() {
    let p = parsed_fixture("output_file");
    insta::assert_snapshot!("output_file_ansi", render_ansi(&p));
}

#[test]
fn render_bearer_auth_plain_redacts_token() {
    let p = parsed_fixture("bearer_auth");
    let out = render_plain(&p);
    // Property check: the raw token must never appear in rendered output.
    assert!(
        !out.contains("tok-abc-1234567890wxyzf3a2"),
        "bearer token leaked into render output: {out}"
    );
    insta::assert_snapshot!("bearer_auth_plain", out);
}
