//! `vet --explain <cmd> [args...]` handler.
//!
//! Per `plans/Overview.md` §4 / [`plans/TestingPlan.md`](plans/TestingPlan.md)
//! §4.2: explain-mode is read-only — it parses the wrapped command,
//! runs the generic risk analyzer, loads the layered allowlist,
//! evaluates the matcher, renders the §8.5 layout to stderr, and
//! prints the policy decision. It never executes the wrapped binary
//! and never has any other side effects.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use vetter_core::matcher::{self, Decision, LoadError};
use vetter_core::{
    analyze,
    parsers::{self, EnvSnapshot, ParseError, StdinHandle},
    AnsiWriter, DefaultRenderer, PlainWriter, Renderer,
};

use crate::color::{self, Style};

/// Exit code for "config / not yet implemented / cannot vet" per
/// `plans/Overview.md` §4. Mirrors `crate::EXIT_CONFIG`.
const EXIT_CONFIG: u8 = 78;

/// Run the explain pipeline.
///
/// `argv` is the full wrapped argv (e.g. `["curl", "-X", "GET", "url"]`).
/// `quiet` suppresses the §8.5 render block but keeps the policy line.
/// `allowlist_override` makes `vet` ignore XDG / project discovery and
/// load the single named file instead.
pub fn run(argv: Vec<String>, quiet: bool, allowlist_override: Option<&Path>) -> ExitCode {
    let cmd = match argv.first() {
        Some(c) => c.as_str(),
        None => {
            eprintln!("vet: --explain requires a command, e.g. `vet --explain curl https://...`");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    let parser = match parsers::dispatch(cmd) {
        Some(p) => p,
        None => {
            eprintln!(
                "vet: no parser registered for `{cmd}`. Phase 1b ships with `curl` only; \
                 see plans/Overview.md §11 for the parser roadmap."
            );
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    let env = EnvSnapshot::from_process();
    let mut parsed = match parser.parse(&argv, StdinHandle::empty(), &env) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("vet: cannot vet `{cmd}` invocation: {}", explain_error(&e));
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    parsed.signals.extend(analyze(&parsed));

    let store = match matcher::load_default(env.cwd.as_deref(), allowlist_override) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vet: allowlist load failed: {}", load_error(&e));
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    let decision = matcher::decide(&parsed, &store);

    let stderr = io::stderr();
    let style = color::pick(&stderr);
    if !quiet {
        if let Err(e) = render_to_stderr(&parsed, Some(&decision), style) {
            eprintln!("vet: render failed: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    }

    let _ = writeln!(io::stderr().lock(), "{}", decision_summary(&decision));

    ExitCode::SUCCESS
}

fn render_to_stderr(
    p: &vetter_core::ParsedCommand,
    outcome: Option<&Decision>,
    style: Style,
) -> io::Result<()> {
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    match style {
        Style::Plain => DefaultRenderer.render(p, outcome, &mut PlainWriter(&mut handle)),
        Style::Ansi => DefaultRenderer.render(p, outcome, &mut AnsiWriter(&mut handle)),
    }
}

fn decision_summary(d: &Decision) -> String {
    match d {
        Decision::Allow { rule_id, scope } => format!(
            "vet: decision = allow (matched rule `{rule_id}` in {} scope)",
            scope.as_str()
        ),
        Decision::Deny { rule_id, scope } => format!(
            "vet: decision = deny (denylist rule `{rule_id}` in {} scope)",
            scope.as_str()
        ),
        Decision::Prompt => {
            "vet: decision = prompt (no rule matched; daemon will escalate when wired)".to_string()
        }
    }
}

/// Stable, human-readable summary of a [`ParseError`] for the CLI's
/// stderr output. Matches the variant names so users can grep them.
fn explain_error(e: &ParseError) -> String {
    match e {
        ParseError::MissingArgument(what) => format!("missing required argument `{what}`"),
        ParseError::ConflictingArgs(detail) => format!("conflicting arguments: {detail}"),
        ParseError::UnknownArgument(name) => format!("unknown argument `{name}`"),
        ParseError::StreamingUnsupported => {
            "streaming bodies are not supported in this MVP (`-T -`, chunked transfer, \
             or `-d @-` over 1 MiB). See plans/Overview.md §8.4."
                .into()
        }
        ParseError::Other(s) => s.clone(),
    }
}

fn load_error(e: &LoadError) -> String {
    match e {
        LoadError::Io { path, source } => format!("read {}: {source}", path.display()),
        LoadError::Yaml { path, source } => format!("parse {}: {source}", path.display()),
        LoadError::DuplicateId { id, path } => {
            format!("duplicate rule id `{id}` in {}", path.display())
        }
    }
}
