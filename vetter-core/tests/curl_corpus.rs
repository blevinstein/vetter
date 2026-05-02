//! Curl parser corpus test driver (TestingPlan §3.1).
//!
//! Walks `tests/corpus/curl/` and runs one of two checks per fixture:
//!
//! - Happy path: `<name>.argv` only.
//!   The parser output is snapshotted as JSON via `insta`. To accept
//!   intentional changes, run with `INSTA_UPDATE=always` and review the
//!   diffs before committing the regenerated `.snap` files.
//!
//! - Negative path: `<name>.argv` + `<name>.error`.
//!   The `.error` file holds the expected `ParseError` variant name
//!   (e.g. `MissingArgument`, `StreamingUnsupported`). The parser must
//!   return an `Err` whose discriminant matches.
//!
//! Each call site is generated from the file system, so adding a new
//! fixture pair under `tests/corpus/curl/` is enough to extend
//! coverage. Fixture lines starting with `#` and blank lines are
//! ignored, so fixtures can carry inline comments.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use vetter_core::parsers::{curl::CurlParser, CommandParser, EnvSnapshot, ParseError, StdinHandle};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/curl")
}

/// Group fixture files by their basename (e.g. `simple_get`) so each
/// pair of `.argv` + (optional) `.error` is processed together. Returns
/// a deterministic ordering so failures are reproducible.
fn group_fixtures() -> BTreeMap<String, FixtureFiles> {
    let dir = corpus_dir();
    let mut groups: BTreeMap<String, FixtureFiles> = BTreeMap::new();
    for entry in fs::read_dir(&dir).expect("corpus dir exists") {
        let path = entry.expect("dirent").path();
        if !path.is_file() {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let slot = groups.entry(stem).or_default();
        match ext {
            "argv" => slot.argv = Some(path),
            "error" => slot.error = Some(path),
            _ => {}
        }
    }
    groups
}

#[derive(Default)]
struct FixtureFiles {
    argv: Option<PathBuf>,
    error: Option<PathBuf>,
}

fn read_argv(path: &Path) -> Vec<String> {
    let body = fs::read_to_string(path).expect("read argv");
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

fn variant_name(err: &ParseError) -> &'static str {
    match err {
        ParseError::MissingArgument(_) => "MissingArgument",
        ParseError::ConflictingArgs(_) => "ConflictingArgs",
        ParseError::UnknownArgument(_) => "UnknownArgument",
        ParseError::StreamingUnsupported => "StreamingUnsupported",
        ParseError::Other(_) => "Other",
    }
}

#[test]
fn corpus_drives_curl_parser() {
    let parser = CurlParser;
    let env = EnvSnapshot::default();
    let groups = group_fixtures();
    assert!(
        !groups.is_empty(),
        "no curl fixtures found in {}",
        corpus_dir().display()
    );

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(corpus_dir().join("snapshots"));
    settings.set_prepend_module_to_snapshot(false);
    let _guard = settings.bind_to_scope();

    for (name, files) in &groups {
        let argv_path = files
            .argv
            .as_ref()
            .unwrap_or_else(|| panic!("fixture `{name}` has no .argv file"));
        let argv = read_argv(argv_path);
        let result = parser.parse(&argv, StdinHandle::empty(), &env);

        match (&files.error, result) {
            (Some(err_path), Ok(parsed)) => panic!(
                "fixture `{name}` was expected to fail (per {}) but parsed: {parsed:#?}",
                err_path.display()
            ),
            (Some(err_path), Err(e)) => {
                let expected = fs::read_to_string(err_path)
                    .expect("read .error")
                    .trim()
                    .to_string();
                let got = variant_name(&e);
                assert_eq!(
                    got, expected,
                    "fixture `{name}` expected ParseError::{expected}, got ParseError::{got} ({e})"
                );
            }
            (None, Err(e)) => panic!("fixture `{name}` expected to parse but failed: {e}"),
            (None, Ok(parsed)) => {
                insta::assert_json_snapshot!(name.as_str(), parsed);
            }
        }
    }
}
