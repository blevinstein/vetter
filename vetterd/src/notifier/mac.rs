//! Real macOS [`Notifier`] backed by `UNUserNotificationCenter`.
//!
//! The actual Cocoa work happens on the main thread inside
//! [`crate::runloop`]; the notifier itself is a thin handle that
//! marshals each [`PromptSummary`] via `dispatch_async(main, ...)`.
//! The shared [`PendingQueue`] lives on the daemon's [`crate::Context`]
//! and is read by the AppDelegate from the
//! `userNotificationCenter:didReceiveNotificationResponse:` callback.
//!
//! Construction is intentionally lightweight (no Cocoa calls) so this
//! is buildable + linkable on bare cargo on macOS without a
//! pre-existing run loop. `vetterd::run` arranges for
//! [`crate::runloop::run_app_kit`] to drive `NSApplication` on the
//! main thread when `VETTERD_NOTIFIER=mac`.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Arc;

use crate::pending::{NotifyHint, PendingQueue, PromptSummary};

use super::{Notifier, NotifierBuildError};

pub struct MacNotifier {
    /// Held only so [`shutdown`](Notifier::shutdown) and the
    /// AppDelegate share the same queue handle. The notifier itself
    /// never resolves entries directly — that's the delegate's job.
    queue: Arc<PendingQueue>,
}

impl MacNotifier {
    /// Construct a Mac notifier handle. Doesn't talk to AppKit yet;
    /// the actual setup happens when [`crate::runloop::run_app_kit`]
    /// brings the AppDelegate up.
    ///
    /// Refuses to install if the running executable is not located
    /// inside a code-signed `.app` bundle — without that ancestor
    /// `UNUserNotificationCenter` silently drops banners, the
    /// menu-bar registration is degraded, and `LSEnvironment` from
    /// the bundle's Info.plist never gets applied. Better to fail
    /// closed at startup than serve a half-working UI. Dev /
    /// non-bundle invocations on macOS must opt out by setting
    /// `VETTERD_NOTIFIER=noop` (or `mock` for tests).
    pub fn install(queue: Arc<PendingQueue>) -> Result<Self, NotifierBuildError> {
        let exe = std::env::current_exe().map_err(|e| {
            NotifierBuildError::Setup(format!(
                "VETTERD_NOTIFIER=mac: cannot read current_exe for bundle \
                 validation: {e}"
            ))
        })?;
        verify_bundle_path(&exe)?;
        Ok(Self { queue })
    }

    pub fn queue(&self) -> &Arc<PendingQueue> {
        &self.queue
    }
}

/// True iff `exe` lives inside a `.app` bundle's standard
/// `Contents/MacOS/` executables directory. Pure so the unit tests
/// can pin both branches without spawning a process.
pub(crate) fn verify_bundle_path(exe: &Path) -> Result<(), NotifierBuildError> {
    if is_app_bundle_executable(exe) {
        return Ok(());
    }
    Err(NotifierBuildError::Setup(format!(
        "VETTERD_NOTIFIER=mac requires running inside a code-signed .app \
         bundle (current executable: {}). Launch the daemon via \
         `open path/to/Vetter.app`, or set `VETTERD_NOTIFIER=noop` to \
         opt out of the macOS UI surface (every prompt-class request \
         will then hang until the daemon is killed).",
        exe.display()
    )))
}

#[cfg(test)]
#[path = "../tests/notifier_mac.rs"]
mod tests;

fn is_app_bundle_executable(exe: &Path) -> bool {
    // Walk the parents looking for `.../<Name>.app/Contents/MacOS/<exe>`.
    // Substring match on `.app/Contents/MacOS/` would technically work
    // but parent walking lets us assert each segment in isolation,
    // which is easier to reason about and test.
    let Some(macos_dir) = exe.parent() else {
        return false;
    };
    if macos_dir.file_name().and_then(|s| s.to_str()) != Some("MacOS") {
        return false;
    }
    let Some(contents_dir) = macos_dir.parent() else {
        return false;
    };
    if contents_dir.file_name().and_then(|s| s.to_str()) != Some("Contents") {
        return false;
    }
    let Some(app_dir) = contents_dir.parent() else {
        return false;
    };
    app_dir
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name.ends_with(".app"))
}

impl Notifier for MacNotifier {
    fn notify(&self, summary: &PromptSummary, hint: NotifyHint) {
        // Coalesce: spec §7 says "if multiple requests are queued,
        // notifications coalesce into the menu-bar popover after
        // the first; we don't spam banners". The
        // `was_empty_before` bit is captured atomically inside the
        // queue's submit critical section, so concurrent submits
        // never both observe "empty" — exactly one banner is
        // raised per burst. Subsequent requests are still visible
        // via the menu-bar badge + popover, which auto-refresh
        // from the queue's change listener.
        if !hint.was_empty_before {
            return;
        }
        crate::runloop::post_notification(
            summary.id.clone(),
            summary.command.clone(),
            summary.primary_verb.clone(),
            summary.primary_target.clone(),
            summary.force_prompt,
        );
    }

    fn shutdown(&self) {
        // The runloop tears its own state down via `cancel_all` once
        // `NSApplication::run()` returns; nothing extra to do here.
    }
}
