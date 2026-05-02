//! `vet` — primary CLI entry point.
//!
//! Phase 0 lays in the full clap subcommand surface from
//! `plans/Overview.md` §4 so later phases can fill in handlers without
//! reshaping the CLI. Only `doctor` runs real logic; everything else
//! exits 78 (config error per §4 conventions) with a `not implemented`
//! message that names the roadmap phase that will land it.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod doctor;

/// Exit code for "config / not yet implemented" per `plans/Overview.md` §4.
const EXIT_CONFIG: u8 = 78;

#[derive(Parser, Debug)]
#[command(
    name = "vet",
    version,
    about = "Local security gate for LLM-agent CLI invocations.",
    long_about = "vet wraps dangerous CLI commands (curl, wget, gh, ...) and \
                  routes them through a layered allowlist + separate-channel \
                  approval UI. See plans/Overview.md."
)]
struct Cli {
    /// Show parse + policy decision; do not exec the wrapped command.
    #[arg(long, global = true)]
    explain: bool,

    /// Always route to the prompt path; never auto-allow.
    #[arg(long, global = true)]
    dry_run: bool,

    /// Suppress the rendered summary (the wrapped command still runs).
    #[arg(long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Diagnose daemon, socket, parsers, and signing status.
    Doctor,

    /// Manage allowlist rules.
    Allow {
        #[command(subcommand)]
        action: AllowAction,
    },

    /// Supervise the vetterd daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Wrap a command: `vet <cmd> [args...]`. Any non-builtin first
    /// positional argument is treated as the command to vet.
    #[command(external_subcommand)]
    Wrap(Vec<String>),
}

#[derive(Subcommand, Debug)]
enum AllowAction {
    /// Add an allowlist rule (YAML pattern as the argument).
    Add { pattern: String },
    /// Remove the allowlist rule with the given id.
    Rm { id: String },
    /// List loaded rules.
    List {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        history: bool,
    },
}

#[derive(Subcommand, Debug)]
enum DaemonAction {
    Start,
    Stop,
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => doctor::run(),
        Command::Allow { .. } => not_implemented("`vet allow`", "Phase 2"),
        Command::Daemon { .. } => not_implemented("`vet daemon`", "Phase 3"),
        Command::Wrap(argv) => {
            let cmd = argv.first().map(String::as_str).unwrap_or("<command>");
            not_implemented(
                &format!("wrapping `{cmd}` (`vet <cmd> [args...]`)"),
                "Phase 1b (curl) / Phase 3 (daemon)",
            )
        }
    }
}

fn not_implemented(what: &str, phase: &str) -> ExitCode {
    eprintln!("vet: {what} is not implemented yet (lands in {phase}; see plans/Overview.md §11).");
    ExitCode::from(EXIT_CONFIG)
}
