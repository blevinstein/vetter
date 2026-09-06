//! Approval-UI surface.
//!
//! The [`Notifier`] trait abstracts "show this prompt to the user".
//! `notify` is fire-and-forget: the trait does not return a decision.
//! Decisions flow back through [`crate::pending::PendingQueue::resolve`]
//! from whatever thread / callback the platform delivers them on
//! (UNUserNotificationCenter delegate, control socket reader, etc.).
//!
//! Three implementations ship with this crate:
//!
//! - [`mac::MacNotifier`] (cfg `target_os = "macos"`) — real
//!   `UNUserNotificationCenter` integration with Approve / Reject
//!   action buttons. Requires the binary to be inside a code-signed
//!   `.app` bundle with `LSUIElement=true`; see `tools/build-app.sh`.
//! - [`linux::LinuxNotifier`] (cfg `target_os = "linux"`) — real
//!   `org.freedesktop.Notifications` integration with the same two
//!   action buttons. Requires a reachable session bus and nothing
//!   else; unlike macOS there is no bundle to validate
//!   (`plans/LinuxApp.md` §5.2).
//! - [`mock::MockNotifier`] — talks to a Unix socket the test harness
//!   listens on, used by `daemon_e2e_prompt.rs` and the
//!   `prompt_class_*` tests in `daemon_e2e.rs`. CI never has a logged-in
//!   GUI session to drive the real notifier, so the mock is the
//!   workhorse for automated coverage.
//!
//! All three are constructed at daemon startup based on the
//! `VETTERD_NOTIFIER` env variable. The default on macOS is `mac` —
//! the daemon expects to run inside a code-signed `Vetter.app` bundle
//! and refuses to start otherwise — and on Linux it is `linux`, which
//! needs only a session bus. Tests and headless environments (CI has
//! neither a GUI session nor a bus) opt into `noop` or `mock`
//! explicitly.

use std::sync::Arc;

use crate::pending::{NotifyHint, PendingQueue, PromptSummary};
use crate::PlatformDriver;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mock;

/// Approval surface. Implementations push the decision back into the
/// shared [`PendingQueue`] through their captured `Arc<PendingQueue>`.
///
/// `notify` MUST NOT block on the user's response — the connection
/// worker is parked on `PendingQueue::submit`'s `Receiver` and the
/// notifier is responsible only for getting the prompt in front of
/// the user (or test driver).
pub trait Notifier: Send + Sync {
    /// Display `summary` to the user. The notifier may spawn threads
    /// or post to a runloop; what it must not do is call
    /// [`PendingQueue::submit`] (already done by the caller) or block
    /// the calling thread waiting for the user.
    ///
    /// `hint` carries advice computed inside the queue's critical
    /// section. The Mac notifier consumes
    /// [`NotifyHint::was_empty_before`] to coalesce banner spam; the
    /// mock and noop implementations ignore it.
    fn notify(&self, summary: &PromptSummary, hint: NotifyHint);

    /// Optional graceful shutdown hook. The default is a no-op; the
    /// mock implementation overrides this so its background reader
    /// thread terminates promptly when the daemon is going down.
    fn shutdown(&self) {}
}

/// Convenience: wrap a notifier in `Arc<dyn Notifier>` to share it
/// across worker threads.
pub fn boxed<N: Notifier + 'static>(n: N) -> Arc<dyn Notifier> {
    Arc::new(n)
}

/// Build the notifier the daemon should use, based on the
/// `VETTERD_NOTIFIER` environment variable. Returns the notifier
/// itself plus the [`PlatformDriver`] the caller should run on the
/// main thread.
///
/// - `mac` (default on macOS) yields [`mac::MacNotifier`] +
///   [`PlatformDriver::AppKit`]. Refuses to install if the
///   executable is not inside a code-signed `.app` bundle (see
///   [`mac::MacNotifier::install`]).
/// - `linux` (default on Linux) yields [`linux::LinuxNotifier`] +
///   [`PlatformDriver::None`] — the bus runs on threads the notifier
///   spawns itself, so the main thread keeps the accept loop. Refuses
///   to install if the session bus is unreachable (see
///   [`linux::LinuxNotifier::install`]).
/// - `mock` yields [`mock::MockNotifier`]; requires
///   `VETTERD_NOTIFIER_SOCKET`. Driver is [`PlatformDriver::None`]
///   (accept loop on main thread).
/// - `noop` yields [`NoopNotifier`]. Driver is
///   [`PlatformDriver::None`].
///
/// Default when unset: see [`default_kind`]. We fail closed: a daemon
/// that can't bring up its UI crashes at startup so the operator gets
/// an exit-78 instead of a silently half-running daemon that hangs
/// every prompt-class request. Tests, non-bundle dev workflows on
/// macOS, and any headless Linux environment must set
/// `VETTERD_NOTIFIER=noop` (or `mock`) explicitly.
pub fn build_from_env(
    queue: Arc<PendingQueue>,
) -> Result<(Arc<dyn Notifier>, PlatformDriver), NotifierBuildError> {
    let kind = resolved_kind();
    match kind.as_str() {
        "mock" => {
            let sock = std::env::var_os("VETTERD_NOTIFIER_SOCKET").ok_or(
                NotifierBuildError::MissingEnv("VETTERD_NOTIFIER_SOCKET (required for mock)"),
            )?;
            let n: Arc<dyn Notifier> = Arc::new(mock::MockNotifier::new(sock.into(), queue));
            Ok((n, PlatformDriver::None))
        }
        "noop" => {
            let n: Arc<dyn Notifier> = Arc::new(NoopNotifier);
            Ok((n, PlatformDriver::None))
        }
        #[cfg(target_os = "macos")]
        "mac" => {
            let n: Arc<dyn Notifier> = Arc::new(mac::MacNotifier::install(queue)?);
            Ok((n, PlatformDriver::AppKit))
        }
        #[cfg(not(target_os = "macos"))]
        "mac" => Err(NotifierBuildError::Unsupported(
            "VETTERD_NOTIFIER=mac requires a macOS target",
        )),
        #[cfg(target_os = "linux")]
        "linux" => {
            let n: Arc<dyn Notifier> = Arc::new(linux::LinuxNotifier::install(queue)?);
            Ok((n, PlatformDriver::None))
        }
        #[cfg(not(target_os = "linux"))]
        "linux" => Err(NotifierBuildError::Unsupported(
            "VETTERD_NOTIFIER=linux requires a Linux target",
        )),
        other => Err(NotifierBuildError::Unknown(other.to_string())),
    }
}

/// The notifier kind this process will actually use: the
/// `VETTERD_NOTIFIER` override if set, else [`default_kind`].
///
/// Split out from [`build_from_env`] because the daemon needs the
/// answer a second time, without building a notifier: the Linux tray
/// (Phase 6c) is a *separate* surface from the notifier but belongs
/// to the same "we have a desktop session" decision, and installing
/// it under `mock` / `noop` would put a tray icon on the screen
/// during every integration test.
pub fn resolved_kind() -> String {
    std::env::var("VETTERD_NOTIFIER").unwrap_or_else(|_| default_kind().to_string())
}

/// Default `VETTERD_NOTIFIER` value when the env var is unset.
///
/// macOS expects the bundled UI and Linux expects a session bus; both
/// fail closed at startup when their precondition is missing, so an
/// operator gets an exit-78 rather than a daemon that silently parks
/// every prompt-class request forever. Every other target falls back
/// to `noop` so their CI smoke checks still come up.
///
/// Consequence worth knowing when writing tests: anything that spawns
/// `vetterd` on Linux — directly, or indirectly via
/// `vet daemon start`, which passes its own environment through —
/// must set `VETTERD_NOTIFIER` to `noop` or `mock`, because CI has no
/// session bus.
const fn default_kind() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "noop"
    }
}

/// Errors returned by [`from_env`]. `vetterd::main` translates these
/// into an exit-78 message.
#[derive(Debug, thiserror::Error)]
pub enum NotifierBuildError {
    #[error("missing required env: {0}")]
    MissingEnv(&'static str),
    #[error("unknown VETTERD_NOTIFIER value `{0}`; expected mac | linux | mock | noop")]
    Unknown(String),
    #[error("notifier not supported on this build: {0}")]
    Unsupported(&'static str),
    #[error("notifier setup failed: {0}")]
    Setup(String),
}

/// Discards every prompt. Pairs with the daemon's `cancel_all` on
/// shutdown so blocked workers wake with deny — useful only on
/// non-macOS builds where neither the real nor mock notifier is
/// configured (the daemon's CI build).
pub struct NoopNotifier;

impl Notifier for NoopNotifier {
    fn notify(&self, _summary: &PromptSummary, _hint: NotifyHint) {}
}
