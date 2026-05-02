//! `vetterd` — long-running per-user daemon backing the `vet` CLI.
//!
//! This crate is a `lib + bin` split so the daemon's pieces are
//! testable in-process. The binary at `src/main.rs` is a thin driver
//! that resolves env-driven paths and calls [`run`].
//!
//! Public surface intentionally minimal:
//! - [`run`] — main entry point, blocks until SIGTERM/SIGINT.
//! - [`Context`] — request-handling context (socket path, audit log,
//!   loaded allowlist store), mostly exposed for tests.
//! - the submodules are `pub` for integration-test reach but the
//!   contract for in-tree consumers is to call [`run`].
//!
//! Shutdown contract: a SIGTERM/SIGINT flips an `AtomicBool`; the
//! accept loop polls it between non-blocking `accept` attempts and
//! tears the socket file down on its way out. This avoids a partial
//! write to the socket inode if the user `Ctrl-C`s during a request.

pub mod audit;
pub mod paths;
pub mod policy;
pub mod socket;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use vetter_core::matcher::{load_default, AllowlistStore};
use vetter_core::parsers::{self, EnvSnapshot, ParseError, StdinHandle};
use vetter_core::pidfile;
use vetter_core::wire::{
    new_request_id, read_request, write_frame, VetDecision, VetRequest, WireDecision, WireError,
    PROTOCOL_VERSION,
};
use vetter_core::{analyze, ParsedCommand};

pub use audit::{AuditEntry, AuditLog};
pub use policy::evaluate;

/// Per-process context shared by every connection worker.
pub struct Context {
    pub socket_path: PathBuf,
    pub audit: Arc<AuditLog>,
    pub allowlist: AllowlistStore,
}

/// Top-level daemon error. Recoverable framing errors are *not*
/// surfaced here — they only kill the offending connection, not the
/// whole daemon.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("socket: {0}")]
    Socket(#[source] std::io::Error),
    #[error("audit log: {0}")]
    Audit(#[source] std::io::Error),
    #[error("pidfile: {0}")]
    Pidfile(#[source] std::io::Error),
    #[error("allowlist: {0}")]
    Allowlist(#[from] vetter_core::matcher::LoadError),
}

/// Start the daemon. Blocks until SIGTERM / SIGINT, then cleans up
/// the socket and returns. The `allowlist_override` parameter is
/// the `--allowlist <path>` test-only escape hatch that bypasses
/// XDG / project discovery.
pub fn run(
    socket_path: PathBuf,
    audit_path: PathBuf,
    allowlist_override: Option<PathBuf>,
) -> Result<(), DaemonError> {
    let allowlist =
        load_default(None, allowlist_override.as_deref()).map_err(DaemonError::Allowlist)?;
    let audit = Arc::new(AuditLog::open(&audit_path).map_err(DaemonError::Audit)?);
    let listener = socket::listen(&socket_path).map_err(DaemonError::Socket)?;

    // Pidfile is co-located with the socket by default. Written
    // *after* the listener binds so a pidfile's existence implies the
    // socket is also live; removed alongside the socket on shutdown so
    // `vet daemon status` never sees a stale pid + missing socket
    // pair on a clean exit.
    let pidfile_path = paths::default_pidfile_path(&socket_path);
    pidfile::write(&pidfile_path, std::process::id(), SystemTime::now())
        .map_err(DaemonError::Pidfile)?;

    let ctx = Arc::new(Context {
        socket_path: socket_path.clone(),
        audit,
        allowlist,
    });

    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handlers(Arc::clone(&shutdown));

    eprintln!("vetterd: listening on {}", socket_path.display());

    let result = accept_loop(listener, ctx, shutdown);

    // Best-effort cleanup. If either file isn't ours (someone
    // replaced it under us) we still don't want to crash on shutdown.
    let _ = std::fs::remove_file(&socket_path);
    pidfile::remove(&pidfile_path);

    result
}

fn install_signal_handlers(shutdown: Arc<AtomicBool>) {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals =
        Signals::new([SIGTERM, SIGINT]).expect("signal-hook should always install on Unix");
    std::thread::spawn(move || {
        if signals.forever().next().is_some() {
            shutdown.store(true, Ordering::SeqCst);
        }
    });
}

fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    ctx: Arc<Context>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), DaemonError> {
    listener
        .set_nonblocking(true)
        .map_err(DaemonError::Socket)?;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                let ctx = Arc::clone(&ctx);
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, &ctx) {
                        eprintln!("vetterd: connection error: {e}");
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Accept poll cadence: 100ms is short enough that
                // SIGTERM feels instant to a human, long enough to
                // keep idle CPU near zero.
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(DaemonError::Socket(e)),
        }
    }
}

/// Handle a single connection: read one [`VetRequest`], re-parse
/// `argv` on the daemon side, evaluate, write one [`VetDecision`],
/// audit, close.
///
/// The daemon does its own parse because the client's view of the
/// world is by definition adversarial-aligned: a same-UID process
/// could submit `argv: ["curl", "https://evil"]` along with `parsed`
/// effects describing a benign call and bypass the matcher. See
/// `plans/ThreatModel.md` T2.
pub fn handle_connection(
    mut stream: std::os::unix::net::UnixStream,
    ctx: &Context,
) -> Result<(), WireError> {
    let req = read_request(&mut stream)?;

    let (decision, reason, command_for_audit) = match parse_request(&req) {
        Ok(parsed) => {
            let (decision, reason) = evaluate(&parsed, req.force_prompt, &ctx.allowlist);
            (decision, reason, parsed.command)
        }
        Err(e) => {
            // Fail closed: a request the daemon cannot parse is one the
            // daemon cannot reason about. Refuse and surface a usable
            // explanation so the user knows what argv tripped us.
            let reason = format!("parse failed: {}", explain_parse_error(&e));
            (WireDecision::Deny, reason, parser_name_hint(&req))
        }
    };

    let resp = VetDecision {
        v: PROTOCOL_VERSION,
        id: req.id.clone(),
        decision,
        reason: reason.clone(),
        rule_added: None,
    };

    // Audit BEFORE replying so the log entry is durable even if the
    // client crashes mid-write. We only need `sync_data`, not the
    // whole-tree fsync; see audit::AuditLog::append.
    let entry = AuditEntry {
        id: req.id.clone(),
        timestamp: timestamp_iso8601(),
        command: command_for_audit,
        argv: req.argv.clone(),
        decision,
        reason,
        rule_id: None,
        force_prompt: req.force_prompt,
    };
    if let Err(e) = ctx.audit.append(&entry) {
        eprintln!("vetterd: audit log append failed: {e}");
    }

    write_frame(&mut stream, &resp)?;
    Ok(())
}

/// Re-run the parser on `req.argv` using the request's `cwd` and the
/// daemon's own environment. The returned [`ParsedCommand`] also
/// carries the generic risk signals so policy / audit see the same
/// picture the renderer would.
///
/// Stdin is intentionally empty here: the v2 wire protocol does not
/// forward stdin bytes, so the daemon parses with [`StdinHandle::empty`].
/// For curl's `-d @-` form this means `Body::FromStdin{len: 0}` —
/// matching what the client's local parser sees and what today's
/// renderer shows. Forwarding stdin (and re-injecting it on exec) is
/// tracked separately; see `TODO.md`.
fn parse_request(req: &VetRequest) -> Result<ParsedCommand, ParseError> {
    let argv0 = req
        .argv
        .first()
        .ok_or_else(|| ParseError::Other("argv is empty".into()))?
        .as_str();
    let parser = parsers::dispatch(argv0)
        .ok_or_else(|| ParseError::Other(format!("no parser registered for `{argv0}`")))?;

    let env = EnvSnapshot {
        vars: std::env::vars().collect(),
        cwd: req.cwd.clone(),
    };
    let mut parsed = parser.parse(&req.argv, StdinHandle::empty(), &env)?;
    parsed.cwd = req.cwd.clone();
    parsed.signals.extend(analyze(&parsed));
    Ok(parsed)
}

/// Best-effort label for the audit log when the parser bails. We
/// prefer the basename of `argv[0]` so log scrapers searching for
/// `"command":"curl"` still find the failed-parse entry.
fn parser_name_hint(req: &VetRequest) -> String {
    req.argv
        .first()
        .map(|p| {
            Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(p)
        })
        .unwrap_or("?")
        .to_string()
}

fn explain_parse_error(e: &ParseError) -> String {
    match e {
        ParseError::MissingArgument(what) => format!("missing required argument `{what}`"),
        ParseError::ConflictingArgs(detail) => format!("conflicting arguments: {detail}"),
        ParseError::UnknownArgument(name) => format!("unknown argument `{name}`"),
        ParseError::StreamingUnsupported => {
            "streaming bodies are not supported (`-T -`, chunked transfer, or `-d @-` over 1 MiB)"
                .into()
        }
        ParseError::Other(s) => s.clone(),
    }
}

/// RFC-3339 timestamp without bringing in chrono. Audit log lines
/// only need to be coarsely time-ordered; for fine-grained order the
/// ULID `id` is monotonic per process.
fn timestamp_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("epoch:{}.{:09}", now.as_secs(), now.subsec_nanos())
}

/// Re-export so binary main.rs can spawn a fresh request id without
/// pulling in `vetter_core::wire` directly.
pub fn fresh_id() -> String {
    new_request_id()
}

/// Helper for tests / external callers that want the same path
/// resolution the binary uses.
pub fn default_socket_path() -> PathBuf {
    paths::default_socket_path()
}

/// Helper for tests / external callers that want the same path
/// resolution the binary uses.
pub fn default_audit_path() -> Result<PathBuf, paths::PathError> {
    paths::default_audit_path()
}

/// Drop-guard so tests that instantiate a [`Context`] manually still
/// see the socket file removed when the test exits.
#[allow(dead_code)]
pub(crate) fn cleanup_socket(p: &Path) {
    let _ = std::fs::remove_file(p);
}

#[cfg(test)]
#[path = "tests/testutil.rs"]
mod testutil;

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;
