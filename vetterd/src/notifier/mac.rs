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

use std::sync::Arc;

use crate::pending::{PendingQueue, PromptSummary};

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
    pub fn install(queue: Arc<PendingQueue>) -> Result<Self, NotifierBuildError> {
        Ok(Self { queue })
    }

    pub fn queue(&self) -> &Arc<PendingQueue> {
        &self.queue
    }
}

impl Notifier for MacNotifier {
    fn notify(&self, summary: &PromptSummary) {
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
