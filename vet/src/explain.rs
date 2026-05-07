//! `vet --explain <cmd> [args...]` handler.
//!
//! Per `plans/Overview.md` §4 / [`plans/TestingPlan.md`](plans/TestingPlan.md)
//! §4.2: explain-mode is read-only — it parses the wrapped command,
//! runs the generic risk analyzer, loads the layered allowlist,
//! evaluates the matcher, renders the §8.5 layout to stderr, and
//! prints the policy decision. It never executes the wrapped binary
//! and never has any other side effects.
//!
//! Stdout discipline (Phase 5.1): explain-mode writes nothing to
//! stdout. The §8.5 render block, the policy line, and every error
//! message all go to stderr so `--explain` composes cleanly inside
//! shell pipelines and never disturbs a downstream `| jq` /
//! `| grep`. Regression-tested in
//! `vet/tests/explain.rs::explain_happy_path_emits_nothing_to_stdout`
//! and its parse-error / no-daemon companions.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use vetter_core::matcher::{self, Decision};
use vetter_core::{
    analyze, check_known_hosts, load_known_hosts_default,
    parsers::{self, EnvSnapshot, StdinHandle},
    AnsiWriter, DefaultRenderer, PlainWriter, Renderer,
};

use crate::color::{self, Style};
use crate::messages::{explain_load_error, explain_parse_error, explain_resolve_error};

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

    // Hybrid argv0 resolution closes ThreatModel.md T4 even on the
    // non-executing `--explain` path: a hardlinked `curl` that points
    // at bash should not be misrendered as a curl invocation.
    let resolved = match parsers::resolve_for_dispatch(cmd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("vet: cannot route `{cmd}`: {}", explain_resolve_error(&e));
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    let parser = resolved.parser;

    let env = EnvSnapshot::from_process();
    let mut parsed = match parser.parse(&argv, StdinHandle::empty(), &env) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "vet: cannot vet `{cmd}` invocation: {}",
                explain_parse_error(&e)
            );
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    parsed.signals.extend(analyze(&parsed));

    let known_hosts = match load_known_hosts_default(env.cwd.as_deref()) {
        Ok(kh) => kh,
        Err(e) => {
            eprintln!("vet: known-hosts load failed: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    parsed
        .signals
        .extend(check_known_hosts(&parsed, &known_hosts));

    let store = match matcher::load_default(env.cwd.as_deref(), allowlist_override) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vet: allowlist load failed: {}", explain_load_error(&e));
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
