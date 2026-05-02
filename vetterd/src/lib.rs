//! `vetterd` — long-running per-user daemon backing the `vet` CLI.
//!
//! This crate is a `lib + bin` split so the daemon's pieces are
//! testable in-process. The binary at `src/main.rs` is a thin driver
//! that resolves env-driven paths and calls [`run`].
//!
//! Public surface intentionally minimal:
//! - [`run`] — main entry point, blocks until SIGTERM/SIGINT.
//! - [`Context`] — request-handling context (socket path, audit log,
//!   loaded allowlist store, pending-queue, notifier handle), mostly
//!   exposed for tests.
//! - the submodules are `pub` for integration-test reach but the
//!   contract for in-tree consumers is to call [`run`].
//!
//! Shutdown contract: a SIGTERM/SIGINT flips an `AtomicBool`; the
//! accept loop polls it between non-blocking `accept` attempts, the
//! pending queue is cancelled so blocked workers wake with deny, and
//! the socket file is torn down on the way out. This avoids a partial
//! write to the socket inode if the user `Ctrl-C`s during a request.
//!
//! Phase 4 added the prompt-class routing: when [`policy::evaluate`]
//! returns [`policy::PolicyOutcome::Prompt`], the connection worker
//! enqueues onto [`pending::PendingQueue`] and the
//! [`notifier::Notifier`] surfaces the prompt to the user (via
//! `UNUserNotificationCenter` on macOS, or a control-socket mock
//! used by tests). The worker blocks on the queue's receiver until
//! the user's decision arrives, then writes the wire reply.

pub mod audit;
pub mod notifier;
pub mod paths;
pub mod pending;
pub mod policy;
#[cfg(target_os = "macos")]
pub mod runloop;
pub mod socket;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use vetter_core::known_hosts::{load_default as load_known_hosts_default, KnownHostsStore};
use vetter_core::matcher::{load_default, AllowlistStore, Decision};
use vetter_core::parsers::{self, EnvSnapshot, ParseError, StdinHandle};
use vetter_core::peer_cred::assert_peer_is_self;
use vetter_core::pidfile;
use vetter_core::render::{AnsiWriter, DefaultRenderer, Renderer};
use vetter_core::wire::{
    new_request_id, read_frame, read_request, write_frame, MgmtRequest, MgmtResponse, PendingItem,
    VetDecision, VetRequest, WireDecision, WireError, PROTOCOL_VERSION,
};
use vetter_core::{analyze, check_known_hosts, ParsedCommand};

pub use audit::{AuditEntry, AuditLog};
pub use pending::{NotifyHint, PendingDecision, PendingQueue, PromptSummary};
pub use policy::{evaluate, PolicyOutcome};

/// Per-process context shared by every connection worker.
pub struct Context {
    pub socket_path: PathBuf,
    pub audit: Arc<AuditLog>,
    pub allowlist: AllowlistStore,
    pub known_hosts: KnownHostsStore,
    pub pending: Arc<PendingQueue>,
    pub notifier: Arc<dyn notifier::Notifier>,
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
    #[error("known-hosts: {0}")]
    KnownHosts(#[from] vetter_core::known_hosts::KnownHostsError),
    #[error("notifier: {0}")]
    Notifier(#[from] notifier::NotifierBuildError),
}

/// Which platform driver to run on the main thread after the accept
/// loop is up. Returned by [`notifier::driver_for_env`] so the daemon
/// knows whether it needs to hand `NSApplication::run()` the main
/// thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformDriver {
    /// Mock / noop notifier; the main thread can run the accept loop
    /// itself.
    None,
    /// macOS `UNUserNotificationCenter` notifier; the main thread
    /// must drive `NSApplication`.
    AppKit,
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
    // Install signal handlers as the very first thing the daemon
    // does, before any IO that could race with a SIGTERM during
    // process startup. signal-hook replaces the default action
    // (terminate) with a pipe write, so SIGTERM after this point
    // just flips the shutdown atomic instead of killing the daemon
    // before its socket / pidfile cleanup can run.
    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handlers(Arc::clone(&shutdown));

    let allowlist =
        load_default(None, allowlist_override.as_deref()).map_err(DaemonError::Allowlist)?;
    let known_hosts = load_known_hosts_default(None).map_err(DaemonError::KnownHosts)?;
    let audit = Arc::new(AuditLog::open(&audit_path).map_err(DaemonError::Audit)?);
    let listener = socket::listen(&socket_path).map_err(DaemonError::Socket)?;

    let admin_socket_path = admin_socket_path_for(&socket_path);
    let admin_listener = socket::listen(&admin_socket_path).map_err(DaemonError::Socket)?;

    // Pidfile is co-located with the socket by default. Written
    // *after* the listener binds so a pidfile's existence implies the
    // socket is also live; removed alongside the socket on shutdown so
    // `vet daemon status` never sees a stale pid + missing socket
    // pair on a clean exit.
    let pidfile_path = paths::default_pidfile_path(&socket_path);
    pidfile::write(&pidfile_path, std::process::id(), SystemTime::now())
        .map_err(DaemonError::Pidfile)?;

    let pending = Arc::new(PendingQueue::new());
    let (notifier, driver) = notifier::build_from_env(Arc::clone(&pending))?;

    let ctx = Arc::new(Context {
        socket_path: socket_path.clone(),
        audit,
        allowlist,
        known_hosts,
        pending: Arc::clone(&pending),
        notifier: Arc::clone(&notifier),
    });

    eprintln!("vetterd: listening on {}", socket_path.display());

    // Admin accept loop runs on a background thread regardless of the
    // platform driver; it only needs the pending queue and shutdown flag.
    let admin_pending = Arc::clone(&pending);
    let admin_shutdown = Arc::clone(&shutdown);
    std::thread::Builder::new()
        .name("vetterd-admin".into())
        .spawn(move || run_admin_loop(admin_listener, admin_pending, admin_shutdown))
        .map_err(DaemonError::Socket)?;

    let result = match driver {
        PlatformDriver::None => {
            // Accept loop runs on the main thread (the historical
            // shape preserved for tests with mock / noop notifier).
            let r = accept_loop(listener, Arc::clone(&ctx), Arc::clone(&shutdown));
            pending.cancel_all();
            r
        }
        #[cfg(target_os = "macos")]
        PlatformDriver::AppKit => run_with_appkit(
            listener,
            Arc::clone(&ctx),
            Arc::clone(&shutdown),
            Arc::clone(&pending),
        ),
        #[cfg(not(target_os = "macos"))]
        PlatformDriver::AppKit => unreachable!(
            "PlatformDriver::AppKit can only be selected on macOS — \
             notifier::build_from_env should refuse this combination"
        ),
    };

    // Notifier shutdown hook (e.g. close mock control listeners).
    notifier.shutdown();

    // Best-effort cleanup. If either file isn't ours (someone
    // replaced it under us) we still don't want to crash on shutdown.
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&admin_socket_path);
    pidfile::remove(&pidfile_path);

    result
}

/// macOS-only main loop. Runs the accept loop on a background thread
/// and drives `NSApplication::run()` on the main thread; on shutdown
/// the runloop helper terminates `NSApp` and we join the accept
/// thread.
#[cfg(target_os = "macos")]
fn run_with_appkit(
    listener: std::os::unix::net::UnixListener,
    ctx: Arc<Context>,
    shutdown: Arc<AtomicBool>,
    pending: Arc<PendingQueue>,
) -> Result<(), DaemonError> {
    let accept_ctx = Arc::clone(&ctx);
    let accept_shutdown = Arc::clone(&shutdown);
    let accept_handle = std::thread::Builder::new()
        .name("vetterd-accept".into())
        .spawn(move || accept_loop(listener, accept_ctx, accept_shutdown))
        .map_err(DaemonError::Socket)?;

    runloop::run_app_kit(Arc::clone(&pending), Arc::clone(&shutdown));

    // AppKit returned: signal the accept loop to wind down and join.
    shutdown.store(true, Ordering::SeqCst);
    pending.cancel_all();
    match accept_handle.join() {
        Ok(r) => r,
        Err(_) => Err(DaemonError::Socket(std::io::Error::other(
            "vetterd-accept thread panicked",
        ))),
    }
}

/// Derive the admin socket path from the main socket path. Co-located
/// so per-test scratch dirs stay isolated. `$VETTERD_ADMIN_SOCKET`
/// overrides for explicit test control.
fn admin_socket_path_for(main: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("VETTERD_ADMIN_SOCKET") {
        return PathBuf::from(p);
    }
    let mut p = main.to_path_buf();
    p.set_file_name("vetter-admin.sock");
    p
}

/// Accept loop for the admin socket. Handles one request at a time
/// inline (no per-connection threads needed — management queries are
/// O(pending queue size) and never block on user input).
fn run_admin_loop(
    listener: std::os::unix::net::UnixListener,
    pending: Arc<PendingQueue>,
    shutdown: Arc<AtomicBool>,
) {
    if listener.set_nonblocking(true).is_err() {
        eprintln!("vetterd: admin: failed to set non-blocking on listener");
        return;
    }
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if assert_peer_is_self(&stream).is_err() {
                    eprintln!("vetterd: admin: rejected connection from foreign UID");
                    continue;
                }
                let req: MgmtRequest = match read_frame(&mut stream) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("vetterd: admin: read error: {e}");
                        continue;
                    }
                };
                let resp = match req {
                    MgmtRequest::ListPending => {
                        let items = pending
                            .pending_summaries()
                            .into_iter()
                            .map(|s| PendingItem {
                                id: s.id,
                                command: s.command,
                                primary_verb: s.primary_verb,
                                primary_target: s.primary_target,
                                force_prompt: s.force_prompt,
                            })
                            .collect();
                        MgmtResponse::PendingList { items }
                    }
                };
                if let Err(e) = write_frame(&mut stream, &resp) {
                    eprintln!("vetterd: admin: write error: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("vetterd: admin: accept error: {e}");
                return;
            }
        }
    }
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
///
/// Prompt-class outcomes (no rule matched, or `--dry-run`) park the
/// worker on [`PendingQueue`] until the [`notifier::Notifier`]
/// surfaces the prompt and the user (or test driver) resolves it.
pub fn handle_connection(
    mut stream: std::os::unix::net::UnixStream,
    ctx: &Context,
) -> Result<(), WireError> {
    let req = read_request(&mut stream)?;

    let (decision, reason, command_for_audit) = match parse_request(&req) {
        Ok(mut parsed) => {
            parsed
                .signals
                .extend(check_known_hosts(&parsed, &ctx.known_hosts));
            let command_for_audit = parsed.command.clone();
            let outcome = evaluate(&parsed, &req.id, req.force_prompt, &ctx.allowlist);
            let (decision, reason) = resolve_outcome(outcome, &parsed, &req, ctx);
            (decision, reason, command_for_audit)
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

/// Map a [`PolicyOutcome`] onto the final wire `(decision, reason)`.
/// For [`PolicyOutcome::Prompt`] this blocks the worker on the
/// pending queue until the notifier delivers a decision (or the
/// queue is cancelled on shutdown, in which case the worker falls
/// back to deny so the agent never hangs forever).
fn resolve_outcome(
    outcome: PolicyOutcome,
    parsed: &ParsedCommand,
    _req: &VetRequest,
    ctx: &Context,
) -> (WireDecision, String) {
    match outcome {
        PolicyOutcome::Auto { decision, reason } => (decision, reason),
        PolicyOutcome::Prompt(summary) => {
            let id = summary.id.clone();
            // Pre-render the §8.5 detail so the popover can display
            // it without the AppKit thread reaching back into
            // vetter-core. We emit ANSI escapes here (not plain) so
            // the popover can re-style each span via
            // `runloop::popover_attr` — same colour taxonomy as
            // `vet --explain`'s TTY output, just translated into
            // `NSAttributedString` attributes on the AppKit side.
            let rendered = render_detail(parsed);
            let (rx, hint) = ctx.pending.submit_with_render(summary.clone(), rendered);
            ctx.notifier.notify(&summary, hint);
            match rx.recv() {
                Ok(dec) => (dec.decision, dec.reason),
                Err(_) => {
                    // Sender was dropped — daemon shutdown wiped the
                    // pending entry before any decision arrived. Fall
                    // back to deny so the agent unblocks. We log here
                    // so the audit reason is not the operator's only
                    // signal.
                    eprintln!(
                        "vetterd: pending queue drained before decision for id `{id}`; \
                         denying request"
                    );
                    (
                        WireDecision::Deny,
                        format!(
                            "{} (pending request `{id}` cancelled)",
                            pending::SHUTDOWN_REASON
                        ),
                    )
                }
            }
        }
    }
}

/// Render the §8.5 detail block for `parsed` into an ANSI-escaped
/// string. Called only on the prompt-class path: the popover parses
/// the SGR escapes back into `NSAttributedString` attributes (see
/// [`runloop::popover_attr`]) so each span keeps its colour /
/// boldness / underline. Any render error is silently swallowed —
/// the caller falls back to the empty-detail card, which the popover
/// renders as a plain monospaced string.
fn render_detail(parsed: &ParsedCommand) -> String {
    let mut buf = Vec::new();
    let mut w = AnsiWriter(&mut buf);
    if DefaultRenderer
        .render(parsed, Some(&Decision::Prompt), &mut w)
        .is_err()
    {
        return String::new();
    }
    String::from_utf8(buf).unwrap_or_default()
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
