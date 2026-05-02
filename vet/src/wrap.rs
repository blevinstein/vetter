//! `vet <cmd> [args...]` handler — the wrap-and-exec hot path.
//!
//! Parses the wrapped command, renders the §8.5 summary to stderr,
//! sends a [`vetter_core::wire::VetRequest`] over the daemon socket,
//! and acts on the [`vetter_core::wire::VetDecision`]:
//!
//! - `Allow`     → `execvp` the wrapped binary.
//! - `Deny`      → exit `77` (`EX_NOPERM`).
//! - `AllowOnce` → in Phase 3a we treat this conservatively as a deny.
//!   The Phase-4 UI will populate session-scope storage that turns it
//!   into a real allow.
//!
//! Fail-closed contract (`plans/TestingPlan.md` §4.9): every code
//! path that does not receive a final `decision: allow` exits without
//! ever calling `Command::exec`. A static check in tests verifies the
//! sentinel marker file is absent in deny / no-daemon paths.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use vetter_core::matcher::{self, LoadError};
use vetter_core::parsers::{self, EnvSnapshot, ParseError, StdinHandle};
use vetter_core::wire::{
    new_request_id, read_decision, write_frame, VetRequest, WireDecision, WireError,
    PROTOCOL_VERSION,
};
use vetter_core::{
    analyze, default_socket_path, AnsiWriter, DefaultRenderer, ParsedCommand, PlainWriter, Renderer,
};

use crate::color::{self, Style};

const EXIT_CONFIG: u8 = 78;
const EXIT_DENY: u8 = 77;

/// Run the full wrap pipeline: parse → render → ask daemon → exec or
/// refuse. `dry_run` flips the wire's `force_prompt` field so the
/// daemon's stub-deny path is taken even if a permissive rule exists.
pub fn run(
    argv: Vec<String>,
    dry_run: bool,
    quiet: bool,
    allowlist_override: Option<&Path>,
) -> ExitCode {
    let cmd = match argv.first() {
        Some(c) => c.as_str(),
        None => {
            eprintln!("vet: a wrapped command is required, e.g. `vet curl https://...`");
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

    // Render BEFORE the daemon round-trip so the user sees what was
    // sent even on a slow/missing daemon. Side-effect-free; the
    // daemon never reuses this rendered text.
    let style = color::pick(&io::stderr());
    if !quiet {
        if let Err(e) = render_to_stderr(&parsed, style) {
            eprintln!("vet: render failed: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    }

    // Allowlist hint passed for parity with --explain. The daemon
    // owns its own copy of the store; this override only takes
    // effect when the daemon was started with VETTER_ALLOWLIST=path.
    // We surface it as a config error if the matcher can't even load
    // the local copy (so `vet` and `vetterd` agree on the rule set).
    if let Some(p) = allowlist_override {
        if let Err(e) = matcher::load_default(env.cwd.as_deref(), Some(p)) {
            eprintln!("vet: --allowlist load failed: {}", load_error(&e));
            return ExitCode::from(EXIT_CONFIG);
        }
    }

    let id = new_request_id();
    let req = VetRequest {
        v: PROTOCOL_VERSION,
        id: id.clone(),
        cwd: env.cwd.clone(),
        agent_hint: detect_agent(&env),
        command: parsed.command.clone(),
        argv: argv.clone(),
        stdin_digest: parsed.stdin_digest.as_ref().map(|d| d.as_str().to_string()),
        parsed,
        force_prompt: dry_run,
    };

    let socket_path = default_socket_path();
    let dec = match round_trip(&socket_path, &req) {
        Ok(d) => d,
        Err(e) => {
            // Fail closed. Any error here — connection refused,
            // truncated frame, daemon panic — exits non-zero and
            // never reaches the exec branch.
            eprintln!("vet: {}", daemon_error(&socket_path, &e));
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    match dec.decision {
        WireDecision::Allow => {
            let _ = writeln!(io::stderr(), "vet: allow ({})", dec.reason);
            // execvp: replaces the current process. On success it
            // never returns; on failure it returns an io::Error and
            // we exit 78 without ever exec'ing.
            let err = Command::new(&argv[0]).args(&argv[1..]).exec();
            eprintln!("vet: failed to exec `{}`: {err}", argv[0]);
            ExitCode::from(EXIT_CONFIG)
        }
        WireDecision::Deny => {
            let _ = writeln!(io::stderr(), "vet: deny ({})", dec.reason);
            ExitCode::from(EXIT_DENY)
        }
        WireDecision::AllowOnce => {
            // Phase 3a has no session storage. The conservative
            // mapping documented in the plan: refuse to exec, exit
            // as deny. Phase 4 swaps this for a real one-shot allow.
            let _ = writeln!(
                io::stderr(),
                "vet: deny (allow_once is not yet supported in Phase 3a; treating as deny)"
            );
            ExitCode::from(EXIT_DENY)
        }
    }
}

fn round_trip(
    socket: &Path,
    req: &VetRequest,
) -> Result<vetter_core::wire::VetDecision, WireError> {
    let mut s = UnixStream::connect(socket)?;
    write_frame(&mut s, req)?;
    read_decision(&mut s, &req.id)
}

fn detect_agent(env: &EnvSnapshot) -> Option<String> {
    // Best-effort sniff. Each agent harness sets at least one
    // distinctive env var; we just return the first that hits.
    for (key, label) in [
        ("CLAUDE_CODE", "claude-code"),
        ("CURSOR_AGENT", "cursor"),
        ("AIDER_MODEL", "aider"),
    ] {
        if env.get(key).is_some() {
            return Some(label.to_string());
        }
    }
    None
}

fn render_to_stderr(p: &ParsedCommand, style: Style) -> io::Result<()> {
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    match style {
        Style::Plain => DefaultRenderer.render(p, None, &mut PlainWriter(&mut handle)),
        Style::Ansi => DefaultRenderer.render(p, None, &mut AnsiWriter(&mut handle)),
    }
}

fn daemon_error(socket: &Path, e: &WireError) -> String {
    match e {
        WireError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            format!(
                "vetterd is not reachable at {}. Start it with `vet daemon start`.",
                socket.display()
            )
        }
        other => format!(
            "daemon protocol error talking to {}: {other}",
            socket.display()
        ),
    }
}

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
        LoadError::Serialize { path, source } => format!("serialise {}: {source}", path.display()),
        LoadError::DuplicateId { id, path } => {
            format!("duplicate rule id `{id}` in {}", path.display())
        }
        LoadError::RuleNotFound { id, path } => {
            format!("no rule with id `{id}` in {}", path.display())
        }
    }
}
