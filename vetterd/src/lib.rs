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
pub mod autostart;
pub mod cards;
pub mod notifier;
pub mod paths;
pub mod pending;
pub mod policy;
#[cfg(target_os = "macos")]
pub mod runloop;
pub mod socket;
pub mod suggestions;
#[cfg(target_os = "linux")]
pub mod tray;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use vetter_core::known_hosts::{load_default as load_known_hosts_default, KnownHostsStore};
use vetter_core::matcher::{load_default, AllowlistStore};
use vetter_core::parsers::{self, EnvSnapshot, ParseError, StdinHandle};
use vetter_core::peer_cred::{assert_peer_is_self, peer_pid};
use vetter_core::pidfile;
// `AnsiWriter` / `DefaultRenderer` / `Renderer` were used to build
// the §8.5 detail block for the popover. The native UI now renders
// every effect as its own AppKit row, so the popover's "Show raw"
// disclosure only needs to surface the original argv. The rich
// renderer is still reachable via `vet --explain` for the CLI; it's
// just no longer the source of the popover body.
//
// We keep this `use` line empty rather than dropping the comment so
// future readers searching for `Renderer` in vetterd land on the
// rationale here.
use vetter_core::wire::{
    new_request_id, read_frame, read_request, write_frame, MgmtRequest, MgmtResponse, PendingItem,
    VetDecision, VetRequest, WireDecision, WireError, PROTOCOL_VERSION,
};
use vetter_core::{analyze, check_known_hosts, ParsedCommand};

pub use audit::{AuditEntry, AuditLog};
pub use pending::{NotifyHint, PendingDecision, PendingQueue, PromptSummary, ResolvedEntry};
pub use policy::{evaluate, PolicyOutcome};

/// Per-process context shared by every connection worker.
///
/// `allowlist` and `known_hosts` are wrapped in `Arc<RwLock<…>>`
/// because Phase-5 admin handlers (`AddRule`, `AddKnownHost`)
/// rewrite the in-memory stores after persisting changes to YAML.
/// Connection workers hold a read lock for the duration of one
/// `evaluate(...)` call (sub-millisecond) so contention with admin
/// writes is irrelevant in practice.
pub struct Context {
    pub socket_path: PathBuf,
    pub audit: Arc<AuditLog>,
    pub allowlist: Arc<RwLock<AllowlistStore>>,
    pub known_hosts: Arc<RwLock<KnownHostsStore>>,
    pub pending: Arc<PendingQueue>,
    pub notifier: Arc<dyn notifier::Notifier>,
    /// `Some(path)` when the daemon was started with
    /// `--allowlist <path>` (test escape hatch). Admin write
    /// handlers persist into this same file rather than the
    /// XDG-discovered user file when set, so integration tests
    /// can sandbox writes inside a tempdir.
    pub allowlist_override: Option<PathBuf>,
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
    #[error("config: {0}")]
    Config(String),
}

/// Default upper bound on simultaneous in-flight connection workers.
/// Each pending request parks one OS thread, so this caps both the
/// thread footprint and the number of half-open / slow-loris peers
/// that can pin workers before the daemon refuses new connections.
/// Override at start-up with `$VETTERD_MAX_INFLIGHT`.
pub const DEFAULT_MAX_INFLIGHT: usize = 16;

/// Read/write deadline applied to the per-connection [`UnixStream`]
/// around the request-frame read and the decision-frame write. The
/// timeout is intentionally short: a well-behaved client writes the
/// request immediately on connect, and the response body is well
/// under a kernel send-buffer's worth of bytes. Anything slower is
/// either a half-open client or a peer wedging the worker on
/// purpose. ThreatModel.md §T3.
const REQUEST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

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

    // Resolve the inflight cap before any IO so a misconfigured
    // operator gets an immediate exit-78 with a clear reason instead
    // of a half-started daemon with a default cap silently in place.
    let max_inflight = max_inflight_from_env()?;
    let inflight = Arc::new(AtomicUsize::new(0));

    let allowlist = Arc::new(RwLock::new(
        load_default(None, allowlist_override.as_deref()).map_err(DaemonError::Allowlist)?,
    ));
    let known_hosts = Arc::new(RwLock::new(
        load_known_hosts_default(None).map_err(DaemonError::KnownHosts)?,
    ));
    let audit = Arc::new(AuditLog::open(&audit_path).map_err(DaemonError::Audit)?);

    // Pidfile is co-located with the socket by default. Acquired
    // *before* the sockets bind so that, once any client successfully
    // connects, the pidfile lock is already held — the
    // `pidfile::read_locker_pid` cross-check in `vet`'s round-trip
    // (see `plans/ThreatModel.md` T1 sequencing #1) cannot race a
    // partially-started daemon. As a side benefit, a duplicate
    // `vetterd` start fails with `WouldBlock` here before doing any
    // IO on the socket paths. The POSIX write-lock (held in
    // `_pidfile_lock` for the daemon's lifetime) is what attestation
    // verifies; closing the fd (drop on shutdown, panic, or
    // `kill -9`) atomically releases the lock, so a lingering
    // pidfile can never look "live" to a client probing via
    // `pidfile::read_locker_pid`. The file itself is removed
    // alongside the sockets in the cleanup block at the end of
    // `run`.
    let pidfile_path = paths::default_pidfile_path(&socket_path);
    let _pidfile_lock = pidfile::acquire(&pidfile_path, std::process::id(), SystemTime::now())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock => DaemonError::Config(format!(
                "another vetterd holds the pidfile lock at {}; \
                     stop it with `vet daemon stop` before starting a new one",
                pidfile_path.display(),
            )),
            _ => DaemonError::Pidfile(e),
        })?;

    let listener = socket::listen(&socket_path).map_err(DaemonError::Socket)?;

    let admin_socket_path = admin_socket_path_for(&socket_path);
    let admin_listener = socket::listen(&admin_socket_path).map_err(DaemonError::Socket)?;

    let pending = Arc::new(PendingQueue::new());

    // Rehydrate the popover's resolved-history ring from the audit
    // log so "Recent" entries survive a daemon restart. Best-effort:
    // a failed read (log file unreadable, torn line, etc.) leaves the
    // ring empty rather than blocking start-up. Must happen before
    // the notifier's change listener is registered by
    // `build_from_env` so the seeded entries don't fire a spurious
    // UI refresh on an already-empty popover.
    match audit.tail_resolved_entries(pending::RESOLVED_CAP) {
        Ok(rows) => {
            pending.warm_resolved(rows.into_iter().filter_map(ResolvedEntry::try_from_audit));
        }
        Err(e) => {
            eprintln!("vetterd: audit log tail (history warm-up) failed: {e}");
        }
    }

    // Bring the OS-level Login Item state in line with the user's
    // persisted preference. Best-effort: a failure here logs but
    // doesn't block startup — the user can always rerun
    // `vet daemon autostart enable` once the popover comes up.
    //
    // Read the file via `load`; if it errors (typo in YAML, IO
    // failure) fall back to the daemon-default `autostart=false`
    // and surface the parse error so the user notices on the next
    // `vet doctor` run.
    let settings = match vetter_core::settings::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("vetterd: ~/.vet/settings.yaml unreadable: {e}");
            vetter_core::settings::Settings::default()
        }
    };
    match autostart::reconcile_with_settings(settings.autostart) {
        Ok(true) => {
            eprintln!(
                "vetterd: autostart reconciled to {} (per ~/.vet/settings.yaml)",
                if settings.autostart {
                    "enabled"
                } else {
                    "disabled"
                }
            );
        }
        Ok(false) => {
            // Already in the desired state; stay quiet to avoid
            // spamming the daemon log on every restart.
        }
        Err(e) => {
            eprintln!(
                "vetterd: autostart reconciliation skipped: {e}; \
                 use `vet daemon autostart status` to inspect current state"
            );
        }
    }

    let (notifier, driver) = notifier::build_from_env(Arc::clone(&pending))?;

    let ctx = Arc::new(Context {
        socket_path: socket_path.clone(),
        audit,
        allowlist,
        known_hosts,
        pending: Arc::clone(&pending),
        notifier: Arc::clone(&notifier),
        allowlist_override: allowlist_override.clone(),
    });

    // Tray item (Phase 6c). Linux only, and only when the real
    // notifier is in play — under `mock` / `noop` we are in a test or
    // a headless box and must not put an icon on anybody's panel.
    //
    // Failure is logged and swallowed on purpose: `plans/LinuxApp.md`
    // §5.5. GNOME ships no StatusNotifierWatcher without an
    // extension, so "no tray" is a supported steady state, and the
    // daemon stays fully usable through notifications and
    // `vet daemon approve`.
    #[cfg(target_os = "linux")]
    let tray = if notifier::resolved_kind() == "linux" {
        match tray::install(Arc::clone(&pending), Arc::clone(&shutdown)) {
            Ok(handle) => Some(handle),
            Err(e) => {
                eprintln!("vetterd: tray unavailable ({e}); continuing without it");
                None
            }
        }
    } else {
        None
    };

    eprintln!("vetterd: listening on {}", socket_path.display());

    // Admin accept loop runs on a background thread; it carries the
    // full daemon `Context` so the Phase-5 suggestion handlers can
    // mutate the allowlist + known-hosts stores.
    let admin_ctx = Arc::clone(&ctx);
    let admin_shutdown = Arc::clone(&shutdown);
    std::thread::Builder::new()
        .name("vetterd-admin".into())
        .spawn(move || run_admin_loop(admin_listener, admin_ctx, admin_shutdown))
        .map_err(DaemonError::Socket)?;

    let result = match driver {
        PlatformDriver::None => {
            // Accept loop runs on the main thread (the historical
            // shape preserved for tests with mock / noop notifier).
            let r = accept_loop(
                listener,
                Arc::clone(&ctx),
                Arc::clone(&shutdown),
                Arc::clone(&inflight),
                max_inflight,
            );
            pending.cancel_all();
            r
        }
        #[cfg(target_os = "macos")]
        PlatformDriver::AppKit => run_with_appkit(
            listener,
            Arc::clone(&ctx),
            Arc::clone(&shutdown),
            Arc::clone(&pending),
            Arc::clone(&inflight),
            max_inflight,
        ),
        #[cfg(not(target_os = "macos"))]
        PlatformDriver::AppKit => unreachable!(
            "PlatformDriver::AppKit can only be selected on macOS — \
             notifier::build_from_env should refuse this combination"
        ),
    };

    // Withdraw the tray item before the notifier goes down so the
    // icon disappears promptly rather than lingering until the
    // process exits (§7 step 15 wants it gone inside ~250 ms).
    #[cfg(target_os = "linux")]
    if let Some(tray) = tray {
        tray.shutdown();
    }

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
    inflight: Arc<AtomicUsize>,
    max_inflight: usize,
) -> Result<(), DaemonError> {
    let accept_ctx = Arc::clone(&ctx);
    let accept_shutdown = Arc::clone(&shutdown);
    let accept_inflight = Arc::clone(&inflight);
    let accept_handle = std::thread::Builder::new()
        .name("vetterd-accept".into())
        .spawn(move || {
            accept_loop(
                listener,
                accept_ctx,
                accept_shutdown,
                accept_inflight,
                max_inflight,
            )
        })
        .map_err(DaemonError::Socket)?;

    runloop::run_app_kit(Arc::clone(&ctx), Arc::clone(&shutdown));

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
    ctx: Arc<Context>,
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
                // The listener is non-blocking; on macOS that flag
                // inherits onto accepted streams. Force blocking so
                // the read/write deadlines below actually fire
                // (SO_RCVTIMEO is a no-op on a non-blocking socket).
                if let Err(e) = stream.set_nonblocking(false) {
                    eprintln!("vetterd: admin: set_nonblocking: {e}");
                    continue;
                }
                // Same read/write deadlines as the main socket: a
                // half-open admin client can't pin the (single)
                // admin handler thread.
                if let Err(e) = stream.set_read_timeout(Some(REQUEST_FRAME_TIMEOUT)) {
                    eprintln!("vetterd: admin: set_read_timeout: {e}");
                    continue;
                }
                if let Err(e) = stream.set_write_timeout(Some(REQUEST_FRAME_TIMEOUT)) {
                    eprintln!("vetterd: admin: set_write_timeout: {e}");
                    continue;
                }
                let req: MgmtRequest = match read_frame(&mut stream) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("vetterd: admin: read error: {e}");
                        continue;
                    }
                };
                let resp = handle_admin_request(&ctx, req);
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

/// Translate one [`MgmtRequest`] into the corresponding
/// [`MgmtResponse`]. Split out so the admin loop's accept/read/write
/// scaffolding stays thin and the per-variant logic is unit-test
/// reachable through `Context` directly.
fn handle_admin_request(ctx: &Arc<Context>, req: MgmtRequest) -> MgmtResponse {
    match req {
        MgmtRequest::ListPending => {
            let items = ctx
                .pending
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
        MgmtRequest::SuggestionsFor { id } => match suggestions::suggestions_for(ctx, &id) {
            Some((allowlist, known_host, _peer_sid)) => MgmtResponse::Suggestions {
                allowlist,
                known_host,
            },
            None => MgmtResponse::Error {
                message: format!("no pending or resolved entry with id `{id}`"),
            },
        },
        MgmtRequest::AddRule { scope, rule } => {
            match suggestions::add_allowlist_rule(ctx, scope, *rule) {
                Ok(added) => MgmtResponse::RuleAdded {
                    id: added.id,
                    scope,
                    auto_approved_ids: added.auto_approved_ids,
                },
                Err(e) => MgmtResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        MgmtRequest::RemoveRule { scope, id } => {
            match suggestions::remove_allowlist_rule(ctx, scope, &id) {
                Ok(()) => MgmtResponse::RuleRemoved { id, scope },
                Err(e) => MgmtResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        MgmtRequest::AddKnownHost { scope, entry } => {
            let pattern = entry.pattern.clone();
            match suggestions::add_known_host(ctx, scope, entry) {
                Ok(()) => MgmtResponse::KnownHostAdded { pattern, scope },
                Err(e) => MgmtResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        MgmtRequest::Resolve {
            id,
            decision,
            reason,
        } => handle_resolve(ctx, &id, decision, reason.as_deref()),
        MgmtRequest::GetAutostart => {
            // Read both: settings.yaml (the user's persisted
            // preference) and SMAppService (the live OS state). The
            // CLI displays both so the user can spot when System
            // Settings flipped the OS state out from under us.
            let desired = vetter_core::settings::load()
                .map(|s| s.autostart)
                .unwrap_or(false);
            MgmtResponse::AutostartState {
                desired,
                status: autostart::current(),
            }
        }
        MgmtRequest::SetAutostart { enabled } => match apply_autostart_change(enabled) {
            Ok(status) => MgmtResponse::AutostartState {
                desired: enabled,
                status,
            },
            Err(e) => MgmtResponse::Error {
                message: e.to_string(),
            },
        },
    }
}

/// Resolve one parked prompt-class request on behalf of an admin
/// client — the daemon half of `vet daemon approve` /
/// `vet daemon reject`.
///
/// Deliberately owns the *policy* around resolving (which decisions
/// are legal here, how ids are matched, what the audit reason says)
/// so every admin client behaves identically and none of it has to
/// be reimplemented in `vet`. The actual state change is the same
/// [`PendingQueue::resolve`] call the macOS UI callbacks make, so
/// an admin-socket approval and a button click are indistinguishable
/// to the blocked connection worker.
fn handle_resolve(
    ctx: &Arc<Context>,
    id: &str,
    decision: WireDecision,
    reason: Option<&str>,
) -> MgmtResponse {
    let verb = match decision {
        WireDecision::Allow => "approved",
        WireDecision::Deny => "rejected",
        // `AllowOnce` is reserved for a one-shot UI affordance that
        // does not exist yet. Silently widening it to `Allow` would
        // be the wrong kind of forgiving on a security gate, so
        // refuse and let the caller pick a real decision.
        WireDecision::AllowOnce => {
            return MgmtResponse::Error {
                message: "allow_once is not a valid decision on the admin socket; \
                          send allow or deny"
                    .into(),
            };
        }
    };

    let pending: Vec<String> = ctx
        .pending
        .pending_summaries()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let full_id = match match_pending_id(&pending, id) {
        Ok(found) => found,
        Err(message) => return MgmtResponse::Error { message },
    };

    // "approved via admin socket" / "rejected via admin socket" are
    // the exact audit strings `plans/Overview.md` and friends have
    // promised since Phase 3. An operator-supplied note is appended
    // rather than substituted so the provenance of the decision is
    // never lost from the log.
    let audit_reason = match reason.map(str::trim) {
        Some(r) if !r.is_empty() => format!("{verb} via admin socket: {r}"),
        _ => format!("{verb} via admin socket"),
    };
    let pending_decision = if matches!(decision, WireDecision::Allow) {
        PendingDecision::allow(audit_reason)
    } else {
        PendingDecision::deny(audit_reason)
    };

    if ctx.pending.resolve(&full_id, pending_decision) {
        MgmtResponse::Resolved {
            id: full_id,
            decision,
        }
    } else {
        // Two ways to land here, and the caller can't act
        // differently on either: the entry was resolved by a racing
        // approver (GUI, notification action, second admin client)
        // between our snapshot and the call above, or it was still
        // present but its connection worker had already gone away.
        // Both mean "your decision did not take effect", which is an
        // error, not a no-op — see `plans/LinuxApp.md` §6a.
        MgmtResponse::Error {
            message: format!(
                "request `{full_id}` is no longer pending \
                 (already resolved, or the waiting client disconnected)"
            ),
        }
    }
}

/// Map a possibly-abbreviated request id onto the full ULID of a
/// pending entry.
///
/// Exact match wins outright, so a caller that pastes a complete
/// ULID never pays for the prefix scan and can never be told its own
/// id is ambiguous. Otherwise the id must prefix exactly one pending
/// entry. On failure returns the message to hand straight to
/// [`MgmtResponse::Error`]; split out from [`handle_resolve`] so the
/// matching rules are unit-testable without standing up a
/// [`Context`].
fn match_pending_id(pending: &[String], id: &str) -> Result<String, String> {
    // ULIDs are Crockford base32 and canonically uppercase, but a
    // shell-history recall or a sloppy copy/paste can lowercase
    // them; the ids are unambiguous either way.
    let needle = id.trim().to_ascii_uppercase();
    if needle.is_empty() {
        return Err("empty request id".into());
    }
    if pending.contains(&needle) {
        return Ok(needle);
    }
    let mut matches: Vec<String> = pending
        .iter()
        .filter(|p| p.starts_with(&needle))
        .cloned()
        .collect();
    match matches.len() {
        0 => Err(format!("no pending request matches id `{id}`")),
        1 => Ok(matches.remove(0)),
        n => {
            // Sorted so the operator sees a stable list they can
            // re-run against; ULIDs sort chronologically.
            matches.sort();
            Err(format!(
                "id `{id}` is ambiguous: it matches {n} pending requests ({list}); \
                 supply more characters",
                list = matches.join(", "),
            ))
        }
    }
}

/// Persist `enabled` to `~/.vet/settings.yaml`, then converge the
/// OS-level Login Item state. Returns the live status *after* the
/// apply so the caller can detect partial success (e.g. the user
/// has not yet approved the registration in System Settings).
///
/// Errors from either step propagate. We persist the YAML *before*
/// touching SMAppService so a Login-Items rejection doesn't lose
/// the user's stated preference — the next daemon launch will
/// retry the registration via [`autostart::reconcile_with_settings`].
fn apply_autostart_change(
    enabled: bool,
) -> Result<vetter_core::settings::AutostartStatus, AutostartChangeError> {
    let mut settings = vetter_core::settings::load().map_err(AutostartChangeError::Settings)?;
    if settings.autostart != enabled {
        settings.autostart = enabled;
        vetter_core::settings::store(&settings).map_err(AutostartChangeError::Settings)?;
    }
    if enabled {
        autostart::enable().map_err(AutostartChangeError::Autostart)?;
    } else {
        autostart::disable().map_err(AutostartChangeError::Autostart)?;
    }
    Ok(autostart::current())
}

#[derive(Debug, thiserror::Error)]
enum AutostartChangeError {
    #[error("settings: {0}")]
    Settings(#[source] vetter_core::settings::SettingsError),
    #[error("{0}")]
    Autostart(#[source] autostart::AutostartError),
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

/// Parse `$VETTERD_MAX_INFLIGHT` if set, otherwise return
/// [`DEFAULT_MAX_INFLIGHT`]. A value of `0`, a non-numeric value, or
/// non-UTF-8 bytes are configuration errors — we fail closed so the
/// operator notices instead of silently running with a bogus cap.
pub(crate) fn max_inflight_from_env() -> Result<usize, DaemonError> {
    match std::env::var("VETTERD_MAX_INFLIGHT") {
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_MAX_INFLIGHT),
        Err(std::env::VarError::NotUnicode(_)) => Err(DaemonError::Config(
            "VETTERD_MAX_INFLIGHT contains non-UTF-8 bytes".into(),
        )),
        Ok(s) => {
            let n: usize = s.parse().map_err(|e| {
                DaemonError::Config(format!(
                    "VETTERD_MAX_INFLIGHT=`{s}` is not a non-negative integer: {e}"
                ))
            })?;
            if n == 0 {
                return Err(DaemonError::Config(format!(
                    "VETTERD_MAX_INFLIGHT must be > 0 (got `{s}`)"
                )));
            }
            Ok(n)
        }
    }
}

/// RAII counter guard. Workers hold one for their whole lifetime; on
/// `Drop` (normal return *or* panic) the live-worker count
/// decrements, so a panicking worker can never permanently steal a
/// slot from the inflight cap.
struct InflightGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    ctx: Arc<Context>,
    shutdown: Arc<AtomicBool>,
    inflight: Arc<AtomicUsize>,
    max_inflight: usize,
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
                // Reserve a slot first; if that puts us over the cap,
                // immediately give it back and drop the stream (the
                // OS sees a connect-then-close so the client gets
                // EOF rather than hanging on read). Reserving before
                // the cap check rather than after avoids a race
                // where two threads both observe `count == cap-1`
                // and both decide they're allowed.
                let prev = inflight.fetch_add(1, Ordering::SeqCst);
                if prev >= max_inflight {
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    eprintln!(
                        "vetterd: dropping connection: {} inflight workers exceeds cap {}",
                        prev + 1,
                        max_inflight
                    );
                    drop(stream);
                    continue;
                }
                let guard = InflightGuard {
                    counter: Arc::clone(&inflight),
                };
                let ctx = Arc::clone(&ctx);
                std::thread::spawn(move || {
                    let _g = guard;
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
    // The listener is non-blocking so the accept loop can poll for
    // shutdown; on macOS that flag inherits onto accepted streams,
    // which would make `SO_RCVTIMEO` a no-op (a non-blocking read
    // returns `WouldBlock` immediately rather than honouring the
    // deadline). Force the stream blocking before arming the
    // timeouts so the deadlines actually bound the read.
    stream.set_nonblocking(false)?;
    // Bound the request-frame read so a peer that connects and never
    // writes can't pin this worker forever. The matching write
    // timeout bounds the response in case the peer stops reading
    // mid-decision. Both deadlines are short on purpose — agents
    // write the request immediately on connect and the decision
    // body fits well inside a kernel send buffer.
    stream.set_read_timeout(Some(REQUEST_FRAME_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_FRAME_TIMEOUT))?;

    // Peer-cred check on the main socket, closing part of the H1
    // backlog item "pairing peer-cred on the main accept_loop
    // (currently absent)": refuse a connection from a foreign UID
    // before reading anything client-controlled off the wire.
    // Same-UID callers pass by definition (same trust level as
    // everything else reachable on this socket) — this only rejects
    // a *different*-UID connector, mirroring the admin socket's
    // existing check.
    assert_peer_is_self(&stream)?;

    // Best-effort: the peer's stable session id powers session-
    // scoped allowlist rules (`Rule::sid`) end to end — both for
    // matching an existing session rule below and for recording on
    // the popover card so "Allowlist… → for this terminal session"
    // has something to persist. Failure (process raced past exit, a
    // permission error, or no tty-anchored ancestor found within the
    // walk's depth cap) degrades to `None`: the request still gets a
    // decision, it just can't use or offer session scope.
    let peer_sid = peer_pid(&stream)
        .ok()
        .and_then(|pid| vetter_core::peer_cred::stable_session_for(pid).ok());

    let req = read_request(&mut stream)?;
    // After the request frame we never read again on this socket;
    // clearing the read timeout is forward-looking (any later
    // protocol that adds a follow-up read should re-arm it
    // explicitly). The write timeout stays in place for write_frame
    // below.
    stream.set_read_timeout(None)?;

    let (decision, reason, command_for_audit, prompt_ctx) = match parse_request(&req) {
        Ok(mut parsed) => {
            // Read-lock both stores for the duration of evaluate;
            // admin write handlers acquire the write lock briefly
            // when persisting + reloading, so contention is bounded
            // by the number of in-flight admin operations (typically
            // zero).
            let allowlist = ctx.allowlist.read().expect("allowlist lock poisoned");
            let known_hosts = ctx.known_hosts.read().expect("known_hosts lock poisoned");
            parsed
                .signals
                .extend(check_known_hosts(&parsed, &known_hosts));
            let command_for_audit = parsed.command.clone();
            let outcome = evaluate(
                &parsed,
                &req.id,
                req.force_prompt,
                &allowlist,
                &known_hosts,
                peer_sid,
            );
            // Build the per-effect popover summary while the
            // known_hosts read lock is still held — it powers the
            // host-trust pills and we want the same one for both
            // prompt-class and matcher-attributed auto-decision
            // paths. `force_prompt` mirrors what `evaluate` would
            // record on its own Prompt summary.
            let auto_summary =
                policy::build_summary(&req.id, &parsed, req.force_prompt, &known_hosts, peer_sid);
            // Drop the read locks before resolve_outcome, which can
            // block the worker on the pending queue waiting for a
            // human decision; holding either lock across that wait
            // would deadlock against admin AddRule / AddKnownHost.
            drop(allowlist);
            drop(known_hosts);
            let (decision, reason, prompt_ctx) =
                resolve_outcome(outcome, &parsed, auto_summary, ctx);
            (decision, reason, command_for_audit, prompt_ctx)
        }
        Err(e) => {
            // Fail closed: a request the daemon cannot parse is one the
            // daemon cannot reason about. Refuse and surface a usable
            // explanation so the user knows what argv tripped us.
            let reason = format!("parse failed: {}", explain_parse_error(&e));
            (WireDecision::Deny, reason, parser_name_hint(&req), None)
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
    //
    // After Phase 5.1 every successful matcher run (prompt-class **or**
    // auto-decision) ships a `PromptContext` carrying the full
    // `PromptSummary`, pre-rendered §8.5 detail, and — for auto rows —
    // the matcher's `rule_id` / `scope` attribution. Those feed the
    // audit log's optional rich fields so the popover's Recent
    // section surfaces auto-allows alongside human prompts and
    // survives a daemon restart (see
    // `AuditLog::tail_resolved_entries` / `PendingQueue::warm_resolved`).
    // Parse-failure rows still leave those fields empty and
    // `skip_serializing_if` keeps them compact on disk.
    let (
        primary_verb,
        primary_target,
        signals,
        parsed_payload,
        host_known,
        rendered,
        rule_id,
        rule_scope,
        peer_sid,
    ) = match prompt_ctx {
        Some(pc) => (
            pc.summary.primary_verb,
            pc.summary.primary_target,
            pc.summary.signals,
            pc.summary.parsed,
            pc.summary.host_known,
            pc.rendered,
            pc.rule_id,
            pc.rule_scope,
            pc.summary.peer_sid,
        ),
        None => (
            String::new(),
            String::new(),
            Vec::new(),
            None,
            Vec::new(),
            String::new(),
            None,
            None,
            None,
        ),
    };

    let entry = AuditEntry {
        id: req.id.clone(),
        timestamp: timestamp_iso8601(),
        command: command_for_audit,
        argv: req.argv.clone(),
        decision,
        reason,
        rule_id,
        rule_scope,
        force_prompt: req.force_prompt,
        primary_verb,
        primary_target,
        signals,
        parsed: parsed_payload,
        host_known,
        rendered,
        peer_sid,
    };
    if let Err(e) = ctx.audit.append(&entry) {
        eprintln!("vetterd: audit log append failed: {e}");
    }

    write_frame(&mut stream, &resp)?;
    Ok(())
}

/// Context captured on every successful matcher run by
/// [`resolve_outcome`] and threaded back to the audit-write site so
/// every UI-bearing row on disk carries the full popover-card
/// payload. After Phase 5.1 this includes auto-decision rows whose
/// matcher attributed the decision to a specific rule (`rule_id` /
/// `rule_scope`); prompt-class rows leave the attribution `None`.
/// Only parse-failure paths return `None` for the entire context.
struct PromptContext {
    summary: PromptSummary,
    rendered: String,
    rule_id: Option<String>,
    rule_scope: Option<vetter_core::matcher::Scope>,
}

/// Map a [`PolicyOutcome`] onto the final wire `(decision, reason)`.
/// For [`PolicyOutcome::Prompt`] this blocks the worker on the
/// pending queue until the notifier delivers a decision (or the
/// queue is cancelled on shutdown, in which case the worker falls
/// back to deny so the agent never hangs forever). For
/// [`PolicyOutcome::Auto`] this also pre-renders the §8.5 detail
/// and pushes the entry onto the resolved-history ring with rule
/// attribution so the popover Recent section surfaces it.
fn resolve_outcome(
    outcome: PolicyOutcome,
    parsed: &ParsedCommand,
    auto_summary: PromptSummary,
    ctx: &Context,
) -> (WireDecision, String, Option<PromptContext>) {
    match outcome {
        PolicyOutcome::Auto {
            decision,
            reason,
            rule_id,
            scope,
        } => {
            // Pre-render the §8.5 detail and push this onto the
            // resolved ring so the popover's Recent section can show
            // the auto-decision alongside human-resolved prompts.
            // Mirrors what the prompt branch does on `submit_with_render`,
            // minus the notifier hop (no human in the loop).
            let rendered = render_detail(parsed);
            ctx.pending.record_auto(
                auto_summary.clone(),
                rendered.clone(),
                decision,
                rule_id.clone(),
                scope,
            );
            let prompt_ctx = PromptContext {
                summary: auto_summary,
                rendered,
                rule_id,
                rule_scope: scope,
            };
            (decision, reason, Some(prompt_ctx))
        }
        PolicyOutcome::Prompt(summary) => {
            // Unbox once: the pending queue and notifier both want
            // `PromptSummary` / `&PromptSummary`, not `Box<…>`.
            // Boxing only matters at the `PolicyOutcome` boundary
            // (clippy's `large_enum_variant` lint).
            let summary: PromptSummary = *summary;
            let id = summary.id.clone();
            // Pre-render the §8.5 detail so the popover can display
            // it without the AppKit thread reaching back into
            // vetter-core. We emit ANSI escapes here (not plain) so
            // the popover can re-style each span via
            // `runloop::popover_attr` — same colour taxonomy as
            // `vet --explain`'s TTY output, just translated into
            // `NSAttributedString` attributes on the AppKit side.
            let rendered = render_detail(parsed);
            let (rx, hint) = ctx
                .pending
                .submit_with_render(summary.clone(), rendered.clone());
            ctx.notifier.notify(&summary, hint);
            let prompt_ctx = PromptContext {
                summary,
                rendered,
                rule_id: None,
                rule_scope: None,
            };
            match rx.recv() {
                Ok(dec) => (dec.decision, dec.reason, Some(prompt_ctx)),
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
                        Some(prompt_ctx),
                    )
                }
            }
        }
    }
}

/// Render the popover's "Show raw" body for `parsed` into an
/// ANSI-escaped string.
///
/// The popover already lays the parsed command out as native AppKit
/// rows (URL row, signal pills, per-effect rows). Echoing the §8.5
/// detail block under "Show raw" was redundant — every line was
/// already present above. We now emit *just* the original argv:
/// command name in bold + each subsequent arg shell-quoted so the
/// raw view stays a faithful, cut-and-pasteable representation of
/// what the agent actually invoked, with no per-effect duplication.
///
/// Output goes through [`runloop::popover_attr`]'s SGR parser, so
/// the bold escape on the command name comes through as the bold
/// attribute on the rendered `NSAttributedString`.
fn render_detail(parsed: &ParsedCommand) -> String {
    if parsed.argv.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(parsed.argv.iter().map(|a| a.len() + 1).sum());
    // SGR `1` = bold. `0` resets so the args after the command name
    // render in the default monospaced weight.
    out.push_str("\x1b[1m");
    out.push_str(&parsed.argv[0]);
    out.push_str("\x1b[0m");
    for arg in &parsed.argv[1..] {
        out.push(' ');
        out.push_str(&shell_quote(arg));
    }
    out
}

/// Shell-quote `arg` for display in the popover's raw view.
///
/// Rules (intentionally narrow — the goal is "looks right when
/// pasted back into a shell" not "round-trips through every shell"):
/// - Empty argv entry → render as `''`.
/// - Strictly POSIX-portable filename chars → no quoting.
/// - Anything else → wrap in single quotes, escaping any embedded
///   `'` as the standard `'\''` (close-quote, escaped-quote,
///   re-open-quote) sequence.
fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    let safe = arg
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':' | b','));
    if safe {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Re-run the parser on `req.argv` using the request's `cwd` and the
/// daemon's own environment. The returned [`ParsedCommand`] also
/// carries the generic risk signals so policy / audit see the same
/// picture the renderer would.
///
/// Stdin is intentionally empty here: the v2 wire protocol does not
/// forward stdin bytes, so the daemon parses with [`StdinHandle::empty`].
/// The curl parser closes the resulting drift by rejecting `-d @-`
/// (and its `--data-*` aliases) outright with
/// `ParseError::StreamingUnsupported` (ThreatModel T9), so the
/// daemon never sees a body sourced from a client-side pipe. Future
/// parsers that genuinely need stdin payloads will require the
/// stdin-forwarding wire bump tracked in `TODO.md`.
fn parse_request(req: &VetRequest) -> Result<ParsedCommand, ParseError> {
    let argv0 = req
        .argv
        .first()
        .ok_or_else(|| ParseError::Other("argv is empty".into()))?
        .as_str();
    // Deliberately uses `parsers::dispatch` (basename match) rather
    // than `parsers::resolve_for_dispatch`. ThreatModel.md T4 (argv0
    // spoofing) is closed client-side in `vet`'s wrap and explain
    // paths, where the resolver gates both the parser dispatch and
    // the eventual `exec`. The daemon never executes the wrapped
    // binary, so the worst a spoofed argv0 can do here is misname an
    // audit row for a request the client will refuse to run anyway.
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
