//! Linux GLib/GTK4 run loop driver.
//!
//! The counterpart to the macOS `runloop` module, and reached by the
//! same path: `crate::runloop` resolves here on Linux and to the
//! AppKit module on macOS, via a `#[path]` mapping in `lib.rs`. The
//! plan's eventual `runloop/mac/` + `runloop/linux/` split (TODO
//! Phase 6 PR 1) would move the AppKit files to match; that move is
//! deliberately not done here because it edits macOS-gated code that
//! cannot be compiled on a Linux box.
//!
//! Owned by the daemon's main thread when the notifier resolves to
//! `linux` *and* a display is reachable. Sets up a `gtk4::Application`
//! with no activation of its own, builds the approval window, and
//! runs the GLib main loop while the accept loop works on a
//! background thread — the mirror image of `run_with_appkit`.
//!
//! Shutdown: SIGTERM/SIGINT flips the daemon's shutdown flag; a 100 ms
//! GLib timeout notices, destroys the window and quits the
//! application, and `run_with_glib` then joins the accept thread.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use gtk4::prelude::*;
use gtk4::Application;

use crate::pending::PendingQueue;

pub(crate) mod model;
mod window;

pub(crate) use window::request_show;

/// Application id. Matches the `.desktop` basename and the tray's
/// `StatusNotifierItem` id so the shell associates window, tray icon
/// and notifications with one application.
const APP_ID: &str = "dev.vetter.daemon";

/// Is there a display this process could actually open a window on?
///
/// Env-only and deliberately cheap: this is called during notifier
/// construction to *choose* a driver, long before it is safe to touch
/// GTK. The real initialisation is attempted in [`run_glib`], which
/// degrades to the plain accept loop if it fails — so a set-but-broken
/// `$DISPLAY` costs a log line, not a daemon.
///
/// The notifier's precondition (a reachable session bus) and the
/// window's (a display) are genuinely different. An SSH session with
/// bus forwarding, or a headless box running a user service, has the
/// former and not the latter: notifications and `vet daemon approve`
/// work there and must keep working, so a missing display is a
/// supported steady state rather than a startup failure — the same
/// stance §5.5 takes for a missing tray.
pub(crate) fn display_available() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// Drive the GTK main loop on the calling thread.
///
/// Returns `Err(())` if GTK could not initialise, which the caller
/// treats as "run the accept loop here instead" rather than as a
/// daemon failure.
pub(crate) fn run_glib(pending: Arc<PendingQueue>, shutdown: Arc<AtomicBool>) -> Result<(), ()> {
    if let Err(e) = gtk4::init() {
        eprintln!(
            "vetterd: GTK could not initialise ({e}); continuing without the \
             approval window — notifications, the tray and `vet daemon approve` \
             are unaffected"
        );
        return Err(());
    }

    // `HANDLES_COMMAND_LINE`-free, activation-free: we build the
    // window ourselves in `startup` rather than letting GTK drive an
    // `activate` cycle, because the daemon is not launched by a
    // desktop file and has no command line of its own to parse.
    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gtk4::gio::ApplicationFlags::IS_SERVICE)
        .build();

    let startup_pending = Arc::clone(&pending);
    let startup_shutdown = Arc::clone(&shutdown);
    app.connect_startup(move |app| {
        window::install(app, Arc::clone(&startup_pending));
        window::watch_shutdown(app, Arc::clone(&startup_shutdown));
    });
    // `IS_SERVICE` still emits `activate` when the application is
    // asked to present itself; route it at the window so a second
    // launch attempt raises ours instead of doing nothing.
    app.connect_activate(|_| window::request_show());

    // Repaint on every queue change, from whichever thread caused it.
    // `add_change_listener` **appends**: `set_change_listener` would
    // silently displace the notifier's banner-closing listener (6b)
    // and the tray's badge (6c), and the symptom would be a stale
    // banner rather than a crash. See `PendingQueue`.
    pending.add_change_listener(window::request_refresh);

    // `run_with_args(&[])` rather than `run()`: `run()` would parse
    // the daemon's own argv as GTK options.
    app.run_with_args::<&str>(&[]);
    Ok(())
}
