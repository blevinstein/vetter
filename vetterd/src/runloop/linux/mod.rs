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

use crate::Context;

pub(crate) mod model;
mod window;

pub(crate) use window::request_show;

/// Why [`run_glib`] did not run a main loop.
///
/// The two cases want opposite handling, which is why this is not a
/// bare `()`. A missing display is a *supported* configuration —
/// notifications and `vet daemon approve` still work, so the caller
/// falls back to the plain accept loop. A name clash means a second
/// daemon is already serving this session, which is a startup error
/// the operator has to resolve.
#[derive(Debug)]
pub(crate) enum GlibError {
    /// GTK could not initialise. Degrade to the accept loop.
    NoDisplay,
    /// Another `vetterd` already owns the application id.
    AlreadyRunning(String),
}

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

/// Is another process already the primary `dev.vetter.daemon`?
///
/// Asked **before** building the `Application`, because letting
/// GApplication discover the clash itself is destructive rather than
/// merely unhelpful. GApplication requests its well-known name with
/// replacement allowed, so a second `vetterd` does not fail to
/// register — it *takes the name from the running daemon*, which
/// then sees `NameLost` and shuts its window driver down. Observed
/// directly: starting a second daemon on a scratch socket killed the
/// live one and exited 0.
///
/// So the ordering matters. A plain `NameHasOwner` on the session bus
/// is read-only and cannot disturb the incumbent. There is a race
/// between the check and the register, but it is narrow and only
/// reachable by two daemons starting within the same instant — which
/// the socket bind already serialises in practice — whereas the
/// behaviour without the check is reliably destructive.
///
/// A bus we cannot query is not treated as "already running": the
/// notifier's own session-bus precondition has already been satisfied
/// to get here, so a failure at this point is unexpected, and failing
/// open costs at worst the pre-existing behaviour.
fn another_instance_owns_the_name() -> bool {
    use gtk4::{gio, glib};

    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    let reply = bus.call_future(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(APP_ID,).into()),
        Some(glib::VariantTy::new("(b)").expect("static signature")),
        gio::DBusCallFlags::NONE,
        1_000,
    );
    // The call is a single synchronous round-trip to the bus daemon
    // during startup, before any main loop exists, so blocking on it
    // here is the whole intent.
    match glib::MainContext::default().block_on(reply) {
        Ok(v) => v.child_value(0).get::<bool>().unwrap_or(false),
        Err(_) => false,
    }
}

/// Drive the GTK main loop on the calling thread.
///
/// Returns `Err(())` if GTK could not initialise, which the caller
/// treats as "run the accept loop here instead" rather than as a
/// daemon failure.
pub(crate) fn run_glib(ctx: Arc<Context>, shutdown: Arc<AtomicBool>) -> Result<(), GlibError> {
    let pending = Arc::clone(&ctx.pending);
    if let Err(e) = gtk4::init() {
        eprintln!(
            "vetterd: GTK could not initialise ({e}); continuing without the \
             approval window — notifications, the tray and `vet daemon approve` \
             are unaffected"
        );
        return Err(GlibError::NoDisplay);
    }

    // Before constructing the Application: a second daemon must not
    // be allowed to steal the name from the running one. See
    // [`another_instance_owns_the_name`]. `vet daemon start` already
    // covers the common case with its socket probe; this catches what
    // gets past it — a stale socket file, or `vetterd` launched
    // directly on a different socket.
    if another_instance_owns_the_name() {
        return Err(GlibError::AlreadyRunning(format!(
            "`{APP_ID}` is already owned on the session bus"
        )));
    }

    // `HANDLES_COMMAND_LINE`-free, activation-free: we build the
    // window ourselves in `startup` rather than letting GTK drive an
    // `activate` cycle, because the daemon is not launched by a
    // desktop file and has no command line of its own to parse.
    // No `IS_SERVICE`. That flag gives GApplication a 10-second
    // inactivity timeout, after which `run` returns on its own and —
    // because `run_with_glib` cannot tell a timeout from a real quit
    // — the entire daemon shuts down gracefully. Measured directly:
    // an untouched daemon exited after ~10s, taking the socket, the
    // tray and the notifier with it, which is the last thing a
    // security gate should do while an agent is waiting on it.
    //
    // Without the flag the application is held by its window, which
    // exists for the daemon's whole life (closing it only hides it),
    // so the lifetime is owned by the shutdown flag alone — SIGTERM,
    // `vet daemon stop`, the tray's Quit, the window's Quit.
    // `g_application_hold` would also stop the timeout, but it makes
    // `quit()` unable to end the loop, and a daemon that ignores
    // SIGTERM is a worse bug than the one being fixed.
    let app = Application::builder().application_id(APP_ID).build();

    // Handlers first, *then* register.
    //
    // `register` emits `startup` synchronously, so connecting
    // afterwards silently skips window creation: the app comes up
    // with no window, nothing holds it alive, and `run` returns
    // immediately. The daemon then exits with no error at all, which
    // makes this ordering the kind of bug you find by bisecting a
    // disappearing process rather than by reading a message.
    let startup_ctx = Arc::clone(&ctx);
    let startup_shutdown = Arc::clone(&shutdown);
    app.connect_startup(move |app| {
        window::install(app, Arc::clone(&startup_ctx), Arc::clone(&startup_shutdown));
        window::watch_shutdown(app, Arc::clone(&startup_shutdown));
    });
    // Something asked the application to present itself — a desktop
    // activation, or a second launch attempt. Route it at the window
    // so it raises ours instead of doing nothing.
    app.connect_activate(|_| window::request_show());

    if let Err(e) = app.register(gtk4::gio::Cancellable::NONE) {
        return Err(GlibError::AlreadyRunning(e.to_string()));
    }

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
