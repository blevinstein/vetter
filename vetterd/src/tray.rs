//! Linux system-tray item (`StatusNotifierItem` + `com.canonical.dbusmenu`).
//!
//! The macOS counterpart is [`crate::runloop::status_item`], an
//! `NSStatusItem` the AppKit runloop owns directly. Linux has no
//! runloop to borrow and no way to draw into the panel ourselves: the
//! icon is rendered by the shell (`plasmashell`, GNOME's appindicator
//! extension, xfce4-statusnotifier-plugin, …) from properties we
//! publish over D-Bus. We describe the item; the host draws it.
//!
//! ## Why `ksni` rather than hand-rolled D-Bus
//!
//! Phase 6b already owns a `zbus` connection, so extending it was the
//! obvious move — but the expensive half of this phase is not
//! `StatusNotifierItem` (a dozen properties and three signals), it is
//! `com.canonical.dbusmenu`: layout revision tracking, `GetLayout`,
//! `GetGroupProperties`, `AboutToShow`, and `ItemsPropertiesUpdated`.
//! Getting the revision semantics subtly wrong yields a menu that
//! silently stops updating, which is both easy to do and hard to
//! notice. `ksni` resolves onto the *same* `zbus` 5.19 we already
//! depend on (so there is one D-Bus crate in the build, not two) and
//! needs no async runtime under `default-features = false` +
//! `["async-io", "blocking"]`. See `plans/LinuxApp.md` §6c.
//!
//! It does open its own session-bus connection, which is the one cost
//! of this choice. D-Bus is designed for that and the connections are
//! independent, but it does mean the tray and the notifier reconnect
//! separately.
//!
//! ## Threading
//!
//! `ksni::blocking::Handle::update` blocks on ksni's executor
//! internally. The queue's change listener runs inline on whatever
//! thread resolved a request — a notifier bus thread, or the admin
//! socket's *single-threaded* handler — so calling `update` from the
//! listener would let a stalled tray wedge `vet daemon approve`. We
//! therefore mirror the notifier's pattern exactly: the listener does
//! nothing but `send` on an `mpsc` channel, and a dedicated thread
//! owns every call into ksni.
//!
//! ## No watcher, no problem
//!
//! GNOME ships no `StatusNotifierWatcher` without an extension, so a
//! missing watcher is a supported steady state, not an error
//! (`plans/LinuxApp.md` §5.5). `assume_sni_available(true)` keeps
//! `spawn` from failing when nothing is registered yet, and the
//! default `watcher_offline` policy keeps the service alive so the
//! item appears if a watcher shows up later. Install failure is
//! logged and swallowed by the caller either way: the notification
//! path and `vet daemon approve` are the load-bearing surfaces, and
//! the daemon stays fully usable with no tray at all.

#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{StandardItem, SubMenu};
use ksni::{Category, Icon, MenuItem, Status, ToolTip, Tray};

use crate::pending::{PendingDecision, PendingQueue, PromptSummary};

/// Item id and icon name. Matches the `.desktop` basename so hosts
/// that map an SNI item back to an application find our entry, and so
/// `Icon=dev.vetter.daemon` resolves once the hicolor SVG is
/// installed.
const TRAY_ID: &str = "dev.vetter.daemon";

/// Audit reasons, in the same shape as Phase 6a's
/// `… via admin socket` and Phase 6b's `… via notification`, so the
/// log says which surface a human actually used.
const REASON_APPROVED: &str = "approved via tray";
const REASON_REJECTED: &str = "rejected via tray";

/// ARGB32 pixmaps published alongside `IconName`.
///
/// Icon-name lookup only resolves once the scalable SVG is installed
/// into a hicolor theme directory, which is *not* true for anyone
/// running out of `target/release` — the overwhelmingly common case
/// during development, and exactly when a missing tray icon is most
/// confusing. Publishing pixmaps as well means the item is visible
/// with nothing installed. Generated from
/// `share/icons/hicolor/scalable/apps/dev.vetter.daemon.svg`; see
/// `tools/` for regeneration if the logo changes.
static ICON_ARGB32_22: &[u8] = include_bytes!("../../assets/tray-icon-22.argb32");
static ICON_ARGB32_44: &[u8] = include_bytes!("../../assets/tray-icon-44.argb32");

/// Longest request label rendered in a menu entry, in characters.
/// URLs are routinely longer than any menu should be; the full target
/// is always available from `vet daemon list`.
const LABEL_MAX_CHARS: usize = 56;

/// Most pending requests given their own submenu. A burst of agent
/// activity can park dozens; past this we stop listing and say how
/// many were elided rather than rendering a menu taller than the
/// screen.
const MAX_MENU_REQUESTS: usize = 10;

// ── Pure model (unit-tested without a bus or a watcher) ─────────────────────

/// One pending request, projected down to just what the menu renders.
/// Snapshotted from the queue so the `Tray` trait's `&self` accessors
/// never take the queue lock while the host is reading properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingCard {
    pub id: String,
    pub label: String,
}

/// Whether the item should ask for the user's attention.
///
/// `Status` is the portable stand-in for the macOS badge: SNI has no
/// numeric badge at all, and while Plasma renders `OverlayIcon`,
/// GNOME's appindicator extension does not reliably. Status plus
/// tooltip text works everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Badge {
    /// Nothing is waiting; the item is present but unobtrusive.
    Idle,
    /// At least one request is parked on a human.
    Attention,
}

/// Flat description of the context menu, lowered to `ksni` types in
/// [`VetterTray::menu`]. Kept separate so the menu's *shape* is
/// unit-testable without a D-Bus host: the assembly is mechanical,
/// the shape is where the logic lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuEntry {
    /// Non-interactive status line ("Pending: 3", "No pending
    /// approvals", "…and 4 more").
    Header(String),
    /// One request, rendered as a submenu holding Approve / Reject.
    /// A submenu rather than two flat items so the top level stays
    /// short and each pair is unambiguously bound to its request.
    Request {
        id: String,
        label: String,
    },
    Separator,
    /// Raise the approval window (Phase 6d). Present only when a
    /// window exists — on a display-less box the daemon runs without
    /// one, and a menu item that does nothing is worse than none.
    OpenWindow,
    Quit,
}

/// Shorten and sanitise a summary into a menu label.
///
/// Runs the argv-derived target through
/// [`vetter_core::render::sanitize_for_display`] for the same reason
/// the notification body does: a hostile URL carrying RTLO or
/// zero-width bytes would otherwise be painted verbatim into the menu
/// a human uses to approve it.
pub(crate) fn card_label(summary: &PromptSummary) -> String {
    use vetter_core::render::sanitize_for_display;
    let verb = sanitize_for_display(&summary.primary_verb);
    let target = sanitize_for_display(&summary.primary_target);
    let command = sanitize_for_display(&summary.command);

    let base = if summary.primary_verb.is_empty() {
        format!("{command} {target}")
    } else {
        format!("{command} {verb} {target}")
    };
    let base = if summary.force_prompt {
        format!("{base} (dry run)")
    } else {
        base
    };
    truncate_chars(&base, LABEL_MAX_CHARS)
}

/// Truncate on a character boundary, appending an ellipsis when the
/// string was actually shortened. Character-wise rather than
/// byte-wise so a multi-byte target can't be cut mid-codepoint.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Tooltip `(title, description)`. The description carries the count,
/// which is this platform's stand-in for the macOS menu-bar badge.
pub(crate) fn tooltip_for(pending: usize) -> (String, String) {
    let description = match pending {
        0 => "No pending approvals".to_string(),
        1 => "1 request waiting for approval".to_string(),
        n => format!("{n} requests waiting for approval"),
    };
    ("Vetter".to_string(), description)
}

/// Attention state for a given pending count.
pub(crate) fn badge_for(pending: usize) -> Badge {
    if pending == 0 {
        Badge::Idle
    } else {
        Badge::Attention
    }
}

/// Build the menu shape for a set of pending requests.
pub(crate) fn menu_model(pending: &[PendingCard], window_available: bool) -> Vec<MenuEntry> {
    let mut out = Vec::with_capacity(pending.len() + 3);
    if pending.is_empty() {
        out.push(MenuEntry::Header("No pending approvals".into()));
    } else {
        out.push(MenuEntry::Header(format!("Pending: {}", pending.len())));
        for card in pending.iter().take(MAX_MENU_REQUESTS) {
            out.push(MenuEntry::Request {
                id: card.id.clone(),
                label: card.label.clone(),
            });
        }
        if let Some(extra) = pending
            .len()
            .checked_sub(MAX_MENU_REQUESTS)
            .filter(|n| *n > 0)
        {
            out.push(MenuEntry::Header(format!("…and {extra} more")));
        }
    }
    if window_available {
        out.push(MenuEntry::Separator);
        out.push(MenuEntry::OpenWindow);
    }
    out.push(MenuEntry::Separator);
    out.push(MenuEntry::Quit);
    out
}

/// Snapshot the queue's pending entries into menu cards, newest last
/// (ULIDs sort chronologically, so this is a plain sort by id).
pub(crate) fn snapshot(queue: &PendingQueue) -> Vec<PendingCard> {
    let mut cards: Vec<PendingCard> = queue
        .pending_summaries()
        .iter()
        .map(|s| PendingCard {
            id: s.id.clone(),
            label: card_label(s),
        })
        .collect();
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    cards
}

// ── Tray item ───────────────────────────────────────────────────────────────

/// The published `StatusNotifierItem`.
///
/// Holds a *snapshot* of the pending queue rather than reading it
/// live: `ksni` invokes the property accessors on its own thread
/// while the host is enumerating, and taking the queue mutex there
/// would couple panel repaints to request resolution.
struct VetterTray {
    queue: Arc<PendingQueue>,
    shutdown: Arc<AtomicBool>,
    cards: Vec<PendingCard>,
    /// Whether this daemon has an approval window to raise. False on
    /// a display-less box, where the driver stayed on
    /// `PlatformDriver::None` and "Open Vetter…" would be a dead item.
    window_available: bool,
}

impl VetterTray {
    fn resolve(&self, id: &str, decision: PendingDecision) {
        // A menu can be acted on after the request behind it is gone
        // (approved from a banner, auto-approved by a new rule, or
        // resolved from another surface) because the host renders the
        // layout it last fetched. `resolve` returning false is
        // therefore an ordinary race, not an error: say so at most
        // once and move on.
        if !self.queue.resolve(id, decision) {
            eprintln!("vetterd: tray: request `{id}` was already resolved; ignoring");
        }
    }
}

impl Tray for VetterTray {
    fn id(&self) -> String {
        TRAY_ID.into()
    }

    fn title(&self) -> String {
        "Vetter".into()
    }

    fn category(&self) -> Category {
        // Not `ApplicationStatus`: vetter is a background gate, not a
        // foreground app the user drives. Plasma groups System items
        // separately and this is where a security daemon belongs.
        Category::SystemServices
    }

    fn status(&self) -> Status {
        match badge_for(self.cards.len()) {
            Badge::Idle => Status::Active,
            Badge::Attention => Status::NeedsAttention,
        }
    }

    fn icon_name(&self) -> String {
        TRAY_ID.into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        icon_pixmaps()
    }

    fn attention_icon_name(&self) -> String {
        // Same artwork; `NeedsAttention` is carried by the status
        // property, which is what hosts actually emphasise. Supplying
        // a name here keeps hosts that swap on attention from falling
        // back to a generic icon.
        TRAY_ID.into()
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        icon_pixmaps()
    }

    fn tool_tip(&self) -> ToolTip {
        let (title, description) = tooltip_for(self.cards.len());
        ToolTip {
            icon_name: TRAY_ID.into(),
            icon_pixmap: Vec::new(),
            title,
            description,
        }
    }

    /// Left-click on the tray icon. The SNI host calls `Activate`;
    /// the spec leaves the meaning to us, and raising the approval
    /// window is the only thing a user plausibly means by clicking a
    /// shield that says "1 request waiting".
    ///
    /// Gated on [`Self::window_available`] for the same reason the
    /// "Open Vetter…" menu item is: on a display-less box the driver
    /// stayed on `PlatformDriver::None`, there is no window to raise,
    /// and silently doing nothing beats pretending otherwise.
    ///
    /// Runs on the ksni thread. `request_show` hops to the GTK thread
    /// itself, so nothing GTK-owned is touched here — same contract
    /// as the menu item's activate closure below.
    fn activate(&mut self, _x: i32, _y: i32) {
        if self.window_available {
            crate::runloop::request_show();
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        menu_model(&self.cards, self.window_available)
            .into_iter()
            .map(|entry| match entry {
                MenuEntry::Header(label) => StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                MenuEntry::Request { id, label } => {
                    let approve_id = id.clone();
                    let reject_id = id;
                    SubMenu {
                        label,
                        submenu: vec![
                            StandardItem {
                                label: "Approve".into(),
                                activate: Box::new(move |t: &mut Self| {
                                    t.resolve(&approve_id, PendingDecision::allow(REASON_APPROVED));
                                }),
                                ..Default::default()
                            }
                            .into(),
                            StandardItem {
                                label: "Reject".into(),
                                activate: Box::new(move |t: &mut Self| {
                                    t.resolve(&reject_id, PendingDecision::deny(REASON_REJECTED));
                                }),
                                ..Default::default()
                            }
                            .into(),
                        ],
                        ..Default::default()
                    }
                    .into()
                }
                MenuEntry::Separator => MenuItem::Separator,
                MenuEntry::OpenWindow => StandardItem {
                    label: "Open Vetter…".into(),
                    activate: Box::new(|_: &mut Self| {
                        // Runs on the ksni thread. `request_show`
                        // hops to the GTK thread itself; nothing
                        // GTK-owned is touched here.
                        crate::runloop::request_show();
                    }),
                    ..Default::default()
                }
                .into(),
                MenuEntry::Quit => StandardItem {
                    label: "Quit Vetter".into(),
                    activate: Box::new(|t: &mut Self| {
                        // Flip the same flag SIGTERM sets and let the
                        // normal shutdown path run: the accept loop
                        // notices within its 100 ms poll, then
                        // `cancel_all` denies anything still parked
                        // and the socket / pidfile are removed. Never
                        // `exit()` from here — that would strand
                        // blocked clients and leak both files.
                        t.shutdown.store(true, Ordering::SeqCst);
                    }),
                    ..Default::default()
                }
                .into(),
            })
            .collect()
    }
}

/// Decode the embedded ARGB32 blobs into `ksni` icons.
fn icon_pixmaps() -> Vec<Icon> {
    [(22, ICON_ARGB32_22), (44, ICON_ARGB32_44)]
        .into_iter()
        .map(|(side, data)| Icon {
            width: side,
            height: side,
            data: data.to_vec(),
        })
        .collect()
}

// ── Installation ────────────────────────────────────────────────────────────

/// Work for the tray thread. Only ksni calls go through here.
enum TrayJob {
    /// Re-snapshot the queue and push new properties to the host.
    Refresh,
    Stop,
}

/// Why the tray could not be published.
///
/// Deliberately our own type rather than re-exporting
/// [`ksni::Error`]: every caller treats this as non-fatal and only
/// wants a message for the log (§5.5), so coupling the daemon to a
/// `#[non_exhaustive]` upstream enum buys nothing.
#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    #[error("publishing the StatusNotifierItem failed: {0}")]
    Publish(#[source] ksni::Error),
    #[error("tray worker thread failed to start: {0}")]
    Worker(#[source] std::io::Error),
}

/// Live tray registration. Dropping it does not remove the item —
/// call [`TrayHandle::shutdown`] on the daemon's way out.
pub struct TrayHandle {
    jobs: Sender<TrayJob>,
    handle: Handle<VetterTray>,
}

impl TrayHandle {
    /// Withdraw the item and stop the worker. The host removes the
    /// icon as soon as our connection drops, which is well inside the
    /// 250 ms `plans/LinuxApp.md` §7 step 15 asks for.
    pub fn shutdown(&self) {
        let _ = self.jobs.send(TrayJob::Stop);
        self.handle.shutdown();
    }
}

/// Publish the tray item and keep it in sync with `queue`.
///
/// Returns `Err` only when the item could not be published at all.
/// Callers must treat that as non-fatal (§5.5): a daemon with no tray
/// is still fully usable through notifications and
/// `vet daemon approve`.
pub fn install(
    queue: Arc<PendingQueue>,
    shutdown: Arc<AtomicBool>,
    window_available: bool,
) -> Result<TrayHandle, TrayError> {
    let tray = VetterTray {
        cards: snapshot(&queue),
        queue: Arc::clone(&queue),
        shutdown,
        window_available,
    };

    // `assume_sni_available(true)`: do not fail when no watcher has
    // registered yet. That is GNOME's out-of-the-box state and a
    // transient state everywhere else during login, and §5.5 requires
    // the daemon to come up regardless.
    let handle = tray
        .assume_sni_available(true)
        .spawn()
        .map_err(TrayError::Publish)?;

    let (tx, rx) = channel::<TrayJob>();
    let worker_handle = handle.clone();
    let worker_queue = Arc::clone(&queue);
    std::thread::Builder::new()
        .name("vetterd-tray".into())
        .spawn(move || {
            for job in rx {
                match job {
                    TrayJob::Refresh => {
                        let cards = snapshot(&worker_queue);
                        // `update` is what makes ksni diff properties
                        // and emit `NewToolTip` / `NewStatus` /
                        // `NewIcon` plus the DBusMenu layout bump.
                        // Setting fields without it would leave the
                        // host showing stale state (§11).
                        if worker_handle.update(|t| t.cards = cards).is_none() {
                            // Service is gone; nothing left to drive.
                            return;
                        }
                    }
                    TrayJob::Stop => return,
                }
            }
        })
        .map_err(TrayError::Worker)?;

    // Appending, not replacing: the notifier registered its own
    // listener in Phase 6b and both must fire. See
    // `PendingQueue::add_change_listener`.
    let listener_tx = tx.clone();
    queue.add_change_listener(move || {
        // Fire-and-forget. This runs inline on the thread that
        // resolved the request — including the single-threaded admin
        // handler — so it must never block or propagate an error.
        let _ = listener_tx.send(TrayJob::Refresh);
    });

    Ok(TrayHandle { jobs: tx, handle })
}

#[cfg(test)]
#[path = "tests/tray.rs"]
mod tests;
