//! `vet --explain <cmd> [args...]` handler.
//!
//! Per `plans/Overview.md` §4 / [`plans/TestingPlan.md`](plans/TestingPlan.md)
//! §4.2: explain-mode is read-only — it parses the wrapped command,
//! runs the generic risk analyzer, renders the §8.5 layout to stderr,
//! and prints what the policy decision would be. It never executes the
//! wrapped binary and never has any other side effects.
//!
//! In Phase 1b there is no daemon and no policy yet, so the decision
//! line just announces that fact. Phase 2 / Phase 3 will replace the
//! placeholder with a real allow/deny/prompt result.

use std::io::{self, Write};
use std::process::ExitCode;

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
pub fn run(argv: Vec<String>, quiet: bool) -> ExitCode {
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

    let stderr = io::stderr();
    let style = color::pick(&stderr);
    if !quiet {
        if let Err(e) = render_to_stderr(&parsed, style) {
            eprintln!("vet: render failed: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    }

    // Phase 1b placeholder: there is no allowlist or daemon to consult
    // yet, so we only state what mode we're in. Phase 2 will replace
    // this line with the actual decision (allow / deny / prompt).
    let _ = writeln!(
        io::stderr().lock(),
        "vet: --explain mode (Phase 1b: no daemon, no policy yet)"
    );

    ExitCode::SUCCESS
}

fn render_to_stderr(p: &vetter_core::ParsedCommand, style: Style) -> io::Result<()> {
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    match style {
        Style::Plain => DefaultRenderer.render(p, &mut PlainWriter(&mut handle)),
        Style::Ansi => DefaultRenderer.render(p, &mut AnsiWriter(&mut handle)),
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
