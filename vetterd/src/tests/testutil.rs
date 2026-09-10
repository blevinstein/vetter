//! Shared helpers for `vetterd` unit tests. Layout convention is
//! described in `AGENTS.md`.

pub(crate) use vetter_core::testutil::sticky_tmpdir;

/// Create a fresh tempdir pinned to `/tmp` instead of `tempfile::tempdir()`.
///
/// `paths::tests` mutates the process-global `TMPDIR` env variable as
/// part of testing the fallback chain. `tempfile::tempdir()` honours
/// `TMPDIR`, which means a parallel `socket::tests` / `audit::tests`
/// could otherwise see a `TMPDIR` pointing at a non-existent path and
/// fail with `NotFound`. Pinning the base directory removes the race
/// without serialising the entire test binary.
pub(crate) fn tmpdir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .expect("create tempdir under /tmp")
}

// ── Card-lowering fixtures ──────────────────────────────────────────────────
//
// Shared by the `cards::*` test modules. They live here rather than
// in one of them because the card view-model is assembled from a
// `PromptSummary` at every layer — url row, effect rows, resolved
// cards, pickers — and four private copies of the same builder would
// drift the moment one of them grew a field.

use vetter_core::{Body, DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy};

use crate::pending::PromptSummary;

/// Minimal pending summary. Fields a given test reads are set
/// explicitly by that test; the rest stay at their empty defaults.
pub(crate) fn summary(id: &str, command: &str, verb: &str, target: &str) -> PromptSummary {
    PromptSummary {
        id: id.into(),
        command: command.into(),
        primary_verb: verb.into(),
        primary_target: target.into(),
        force_prompt: false,
        signals: Vec::new(),
        parsed: None,
        host_known: Vec::new(),
        peer_sid: None,
    }
}

pub(crate) fn request(method: HttpMethod, url: &str) -> HttpRequest {
    HttpRequest {
        method,
        url: url.parse().expect("test url parses"),
        headers: Vec::new(),
        body: Body::None,
        auth: None,
        tls: TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    }
}

pub(crate) fn parsed(effects: Vec<Effect>) -> ParsedCommand {
    ParsedCommand {
        command: "curl".into(),
        argv: vec!["curl".into()],
        cwd: None,
        effects,
        signals: Vec::new(),
        display_hints: DisplayHints::default(),
        extras: serde_json::Value::Null,
    }
}

/// Summary carrying a parsed HTTP request, which is what the URL row
/// and the effect rows are built from.
pub(crate) fn http_summary(id: &str, method: HttpMethod, url: &str) -> PromptSummary {
    let mut s = summary(id, "curl", method.as_str(), url);
    s.parsed = Some(parsed(vec![Effect::HttpRequest(request(method, url))]));
    s.host_known = vec![false];
    s
}

/// One risk signal. Used by the pill and picker tests, which both ask
/// "what does the card do when this kind is present".
pub(crate) fn signal(kind: vetter_core::SignalKind, detail: &str) -> vetter_core::RiskSignal {
    vetter_core::RiskSignal {
        kind,
        detail: detail.into(),
        effect_idx: None,
    }
}

/// Lower a summary with an empty §8.5 body — the shape most card
/// tests want, since they assert on rows and tones rather than raw.
pub(crate) fn card_of(s: &PromptSummary) -> crate::cards::card::CardView {
    crate::cards::card::card_view(s, "")
}
