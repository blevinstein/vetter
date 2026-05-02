//! `vet` — primary CLI entry point.
//!
//! Lays out the full clap subcommand surface from `plans/Overview.md`
//! §4 and dispatches to the per-command handler modules. Each handler
//! returns its own `ExitCode` so this file stays a thin router.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod allow;
mod color;
mod daemon;
mod doctor;
mod explain;
mod wrap;

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

    /// Override allowlist file. Bypasses XDG / project discovery —
    /// the named file becomes the sole rule source. Useful for tests
    /// and ad-hoc inspection.
    #[arg(long, global = true, value_name = "PATH")]
    allowlist: Option<PathBuf>,

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
    Add {
        pattern: String,
        /// Which scope to write to. Default `user`. Ignored when
        /// `--allowlist <path>` is given.
        #[arg(long, value_enum, default_value_t = AllowScope::User)]
        scope: AllowScope,
    },
    /// Remove the allowlist rule with the given id.
    Rm {
        id: String,
        /// Which scope to remove from. Default `user`. Ignored when
        /// `--allowlist <path>` is given.
        #[arg(long, value_enum, default_value_t = AllowScope::User)]
        scope: AllowScope,
    },
    /// List loaded rules.
    List {
        /// Restrict the listing to one source layer.
        #[arg(long, value_enum)]
        scope: Option<AllowScope>,
        /// Print the audit log instead of the rule set (Phase 3).
        #[arg(long)]
        history: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AllowScope {
    User,
    Project,
}

#[derive(Subcommand, Debug)]
enum DaemonAction {
    Start,
    Stop,
    Status,
}

fn main() -> ExitCode {
    vetter_core::parsers::register_builtins();
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => doctor::run(),
        Command::Allow { action } => match action {
            AllowAction::Add { pattern, scope } => {
                allow::add(&pattern, scope, cli.allowlist.as_deref())
            }
            AllowAction::Rm { id, scope } => allow::rm(&id, scope, cli.allowlist.as_deref()),
            AllowAction::List { scope, history } => {
                allow::list(scope, history, cli.allowlist.as_deref())
            }
        },
        Command::Daemon { action } => match action {
            DaemonAction::Start => daemon::start(),
            DaemonAction::Stop => daemon::stop(),
            DaemonAction::Status => daemon::status(),
        },
        Command::Wrap(argv) => {
            if cli.explain {
                return explain::run(argv, cli.quiet, cli.allowlist.as_deref());
            }
            wrap::run(argv, cli.dry_run, cli.quiet, cli.allowlist.as_deref())
        }
    }
}
