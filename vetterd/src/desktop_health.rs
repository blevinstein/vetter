//! Live probes of the desktop services the Linux UI depends on.
//!
//! Phase 6e. `vet doctor` wants to tell a user *which* piece of their
//! desktop is missing, because several of the missing states are
//! supported steady states rather than errors: a session with no
//! `StatusNotifierWatcher` (GNOME, out of the box) still gets
//! notifications and `vet daemon approve`; a notification server
//! without `actions` (`notify-osd`) still raises banners and the body
//! click still opens the window. Saying "no tray" is useful; saying
//! "broken" would be wrong.
//!
//! ## Why this lives in the daemon
//!
//! Answering these questions means calling D-Bus, and `vet` has no
//! D-Bus library — deliberately. `vet` is exec'd on the hot path for
//! every wrapped command, and linking a bus stack into it to service
//! one diagnostic subcommand would tax every invocation to pay for
//! `vet doctor`. The daemon already holds a `zbus` connection, so the
//! doctor asks it over the admin socket, exactly as the autostart row
//! already does for `SMAppService` state.
//!
//! The cost is that these rows go dark when the daemon is down. That
//! is honest — and `vet doctor` reports the daemon's own state one
//! row above, so the user is never left guessing why.
//!
//! ## Probing rules
//!
//! Every probe here fails *closed*: an error talking to the bus
//! reports the capability as absent or unknown, never as present. A
//! diagnostic that reports a capability it did not actually observe
//! is worse than one that admits it could not look.

#![cfg(target_os = "linux")]

use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};

use vetter_core::wire::DesktopHealth;

/// Well-known names we probe for.
const NOTIFY_NAME: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const KDE_WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const KDE_WATCHER_PATH: &str = "/StatusNotifierWatcher";
const FDO_WATCHER_NAME: &str = "org.freedesktop.StatusNotifierWatcher";

/// Probe the session bus and the two services the UI rides on.
///
/// Opens its own short-lived connection rather than borrowing the
/// notifier's: this runs on the admin-socket handler thread, the
/// notifier's connection is owned by threads with their own job
/// queues, and a diagnostic must never be able to wedge the live UI
/// path by contending for it.
pub fn probe() -> DesktopHealth {
    let Ok(conn) = Connection::session() else {
        // No reachable session bus. Everything downstream is
        // unavailable by construction, and the daemon would not be
        // running the `linux` notifier at all — so report the floor
        // rather than probing further.
        return DesktopHealth {
            session_bus: false,
            notification_server: None,
            notification_actions: false,
            tray_watcher: false,
            tray_host: false,
        };
    };

    let (notification_server, notification_actions) = probe_notifications(&conn);
    let (tray_watcher, tray_host) = probe_tray(&conn);

    DesktopHealth {
        session_bus: true,
        notification_server,
        notification_actions,
        tray_watcher,
        tray_host,
    }
}

/// `(server name, advertises `actions`)`.
///
/// The name comes from `GetServerInformation` so the row can say
/// *which* server is running — "Plasma" and "dunst" have visibly
/// different capability sets and knowing which one answered is half
/// the diagnosis.
fn probe_notifications(conn: &Connection) -> (Option<String>, bool) {
    if !name_has_owner(conn, NOTIFY_NAME) {
        return (None, false);
    }
    let Ok(proxy) = Proxy::new(conn, NOTIFY_NAME, NOTIFY_PATH, NOTIFY_NAME) else {
        return (None, false);
    };
    // `GetServerInformation` returns (name, vendor, version, spec).
    let name = proxy
        .call::<_, _, (String, String, String, String)>("GetServerInformation", &())
        .ok()
        .map(|(name, ..)| name);
    let actions = proxy
        .call::<_, _, Vec<String>>("GetCapabilities", &())
        .map(|caps| caps.iter().any(|c| c == "actions"))
        .unwrap_or(false);
    (name, actions)
}

/// `(a watcher owns the name, a host is registered with it)`.
///
/// Both halves matter and they are not the same question. KDE
/// registers the watcher *and* a host. A GNOME session with the
/// appindicator extension installed but disabled can leave a watcher
/// with no host, in which case our tray item registers successfully
/// and is then drawn by nobody — which looks, to a user, exactly like
/// a bug in Vetter.
fn probe_tray(conn: &Connection) -> (bool, bool) {
    let (name, path) = if name_has_owner(conn, KDE_WATCHER_NAME) {
        (KDE_WATCHER_NAME, KDE_WATCHER_PATH)
    } else if name_has_owner(conn, FDO_WATCHER_NAME) {
        (FDO_WATCHER_NAME, KDE_WATCHER_PATH)
    } else {
        return (false, false);
    };
    let host = Proxy::new(conn, name, path, KDE_WATCHER_NAME)
        .ok()
        .and_then(|p| {
            p.get_property::<bool>("IsStatusNotifierHostRegistered")
                .ok()
        })
        .unwrap_or(false);
    (true, host)
}

/// `org.freedesktop.DBus.NameHasOwner`, false on any error.
fn name_has_owner(conn: &Connection, name: &str) -> bool {
    DBusProxy::new(conn)
        .ok()
        .and_then(|p| name.try_into().ok().map(|n| (p, n)))
        .and_then(|(p, n)| p.name_has_owner(n).ok())
        .unwrap_or(false)
}
