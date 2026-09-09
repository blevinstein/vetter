//! GTK4 approval window — widget assembly only.
//!
//! Every decision this file could make has already been made in
//! [`super::model`]; what is left is turning `CardView`s into
//! widgets. Keeping it that thin is what makes the window testable at
//! all, since CI has no display.
//!
//! ## Why a window and not a popover
//!
//! macOS anchors its approval UI to the menu-bar status item with
//! `NSPopover`. Wayland has no equivalent and cannot have one: a
//! client cannot place a surface at absolute screen coordinates, and
//! `xdg_positioner` anchors only against the client's *own* surfaces.
//! Our tray icon is not our surface — `plasmashell` draws it from the
//! `StatusNotifierItem` properties we publish, and no protocol tells
//! us where. So this is a plain `ApplicationWindow` the compositor
//! places (`plans/LinuxApp.md` §5.1).
//!
//! ## Threading
//!
//! GObject is single-threaded and none of these types are `Send`.
//! The queue's change listener, though, fires on whichever thread
//! resolved a request — a zbus signal thread, the ksni thread, or the
//! admin socket's single-threaded handler. Nothing outside this
//! module may touch a widget.
//!
//! The hop is [`glib::MainContext::invoke`], which is safe to call
//! from any thread and dispatches onto the thread running the default
//! main context (ours). The closures it carries capture **nothing**,
//! so they are trivially `Send`; the window they act on is reached
//! through the [`WINDOW`] thread-local, which only ever has a value
//! on the GTK thread. That is the property that makes this sound:
//! there is no handle to a widget anywhere a non-GTK thread could
//! reach it. Same shape as macOS's `MainThreadBound`.
//!
//! ## Text safety
//!
//! Card text is argv-derived, so it is sanitised in
//! [`super::model`] / `crate::cards` (RTLO, zero-width, control
//! bytes) before it arrives. Here the remaining question is markup:
//! `set_markup` parses Pango's XML-ish syntax, so it is used in
//! exactly one place — the raw body, which genuinely needs per-run
//! colour — and only over text [`super::model::spans_to_markup`] has
//! escaped. Everything else uses `set_text`, where no escaping
//! question arises at all. Colour that would otherwise tempt a
//! `set_markup` (method tone, host trust, pill tone) is carried by
//! CSS classes on separate labels instead.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gtk4::gdk::Display;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, CssProvider,
    Expander, Frame, Label, Orientation, PolicyType, ScrolledWindow, Separator, Window,
};

use vetter_core::known_hosts::KnownHostEntry;
use vetter_core::matcher::Rule;
use vetter_core::wire::WireScope;

use super::model::{self, CardAction, Disclosure, MarkupPalette};
use crate::cards::card::{self, CardView, HttpUrlView, UrlView};
use crate::cards::picker::{self, PickerKind, PickerRow};
use crate::cards::pills::{PillSpec, Tone};
use crate::cards::resolved::{self, ResolvedCardView, RuleAttribution};
use crate::cards::rows::{BodyContent, EffectRow, FilePath};
use crate::cards::rules::DurationChoice;
use crate::cards::url::{HostTrust, MethodTone};
use crate::pending::PendingQueue;
use crate::{suggestions, Context};

/// Window chrome sizing. Wide enough for a realistic URL without
/// wrapping, tall enough for three cards before scrolling.
const WINDOW_WIDTH: i32 = 560;
const WINDOW_HEIGHT: i32 = 520;
const GUTTER: i32 = 12;
/// `plans/ApprovalUI.md` "Card chrome": 16pt between cards, with a
/// separator rule between adjacent ones.
const CARD_SPACING: i32 = 16;

/// Palettes for the raw body's Pango markup.
///
/// Two are needed because a single set of hex values cannot stay
/// legible across themes: the mid-tones that read on white wash out
/// on a dark surface, and vice versa. macOS sidesteps this by pinning
/// the popover to Dark Aqua (`plans/ApprovalUI.md` "Popover
/// appearance"); a GTK window that ignored the user's theme would
/// look broken next to every other app on the desktop, so we track it
/// instead. Values are GNOME palette steps, picked for contrast
/// against the default light (#fafafa) and dark (#242424) grounds.
const LIGHT_PALETTE: MarkupPalette = MarkupPalette {
    red: "#c01c28",
    green: "#26794f",
    yellow: "#a35a00",
    magenta: "#7a2f8f",
    cyan: "#00707a",
    blue: "#1a5fb4",
    dim: "#5e5c64",
};
const DARK_PALETTE: MarkupPalette = MarkupPalette {
    red: "#f66151",
    green: "#57e389",
    yellow: "#f8e45c",
    magenta: "#dc8add",
    cyan: "#33d1c9",
    blue: "#62a0ea",
    dim: "#9a9996",
};

/// Styling the stock GTK classes do not cover: tinted pill capsules
/// and the dry-run frame.
///
/// Pill backgrounds are the `plans/ApprovalUI.md` "Tinted pill
/// recipe" translated to CSS — a low-alpha tint of the foreground, so
/// one rule works on both light and dark grounds without a second
/// palette. `alpha()` is GTK's own CSS function, so the tint tracks
/// whatever the theme resolves the colour to.
const CSS: &str = "
.vetter-pill {
    border-radius: 7px;
    padding: 1px 7px;
    font-size: 0.8em;
    font-weight: bold;
}
.vetter-pill.danger  { color: @error_color;   background: alpha(@error_color, 0.22); }
.vetter-pill.warn    { color: @warning_color; background: alpha(@warning_color, 0.22); }
.vetter-pill.positive{ color: @success_color; background: alpha(@success_color, 0.22); }
.vetter-pill.neutral { color: @insensitive_fg_color; background: alpha(@insensitive_fg_color, 0.18); }
.vetter-method { font-weight: bold; }
.vetter-method.read        { color: @success_color; }
.vetter-method.write       { color: @warning_color; }
.vetter-method.destructive { color: @error_color; }
.vetter-method.other       { color: @accent_color; }
.vetter-dryrun { border: 1.5px solid @warning_color; border-radius: 6px; }
.vetter-raw { font-family: monospace; font-size: 0.9em; }
.vetter-toast { border-radius: 6px; padding: 8px 10px; }
.vetter-toast.ok    { background: alpha(@success_color, 0.18); }
.vetter-toast.error { background: alpha(@error_color, 0.20); }
";

// GTK-thread-only handle to the live window.
//
// A thread-local rather than a global is the whole safety argument
// (see the module docs): a non-GTK thread that somehow ran this code
// would observe `None` rather than a widget it must not touch.
thread_local! {
    static WINDOW: RefCell<Option<WindowState>> = const { RefCell::new(None) };
    /// Which disclosures are open, keyed by request id.
    ///
    /// Deliberately a *separate* thread-local from [`WINDOW`] rather
    /// than a field on `WindowState`: the expander's toggle handler
    /// needs `&mut` to this while `refresh` is holding a shared
    /// borrow of `WINDOW`, and splitting them keeps that from being a
    /// `RefCell` double-borrow panic waiting to happen.
    static EXPANDED: RefCell<model::ExpandedState> =
        RefCell::new(model::ExpandedState::default());
    /// Latest result banner, shown at the top of the list until
    /// dismissed or superseded. Separate cell for the same
    /// double-borrow reason as [`EXPANDED`]: its Dismiss button
    /// mutates it while `refresh` holds `WINDOW`.
    static TOAST: RefCell<Option<Toast>> = const { RefCell::new(None) };
    /// Card widget per request id, rebuilt by every [`refresh`].
    ///
    /// Exists only so a notification click can scroll its card into
    /// view: the id arrives from the bus, and this is what turns it
    /// into a widget whose position the scroller can be pointed at.
    /// A separate cell again — `refresh` populates it while holding a
    /// shared borrow of [`WINDOW`].
    static CARD_WIDGETS: RefCell<HashMap<String, gtk4::Widget>> =
        RefCell::new(HashMap::new());
}

/// Set once the window exists, and readable from any thread.
///
/// [`WINDOW`] is a thread-local, so a non-GTK thread asking it "is
/// there a window?" would always be told no. The admin socket's
/// handler needs a truthful answer from its own thread — a
/// display-less daemon must report that `vet daemon open` has nothing
/// to open rather than silently succeeding — so the fact is mirrored
/// here.
static WINDOW_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Is there an approval window for [`request_show`] to raise?
///
/// False on a daemon that came up without a display, where the driver
/// stayed on [`crate::PlatformDriver::None`].
pub(crate) fn window_available() -> bool {
    WINDOW_INSTALLED.load(Ordering::SeqCst)
}

/// Results handed back from background persist threads.
///
/// A plain `static` rather than a channel because of the constraint
/// that shapes this whole module: the closure a worker thread gives
/// to `MainContext::invoke` must capture **nothing**, so it cannot
/// carry a payload. The worker parks its result here, then invokes a
/// capture-free drain function that moves it into the GTK-thread-only
/// [`TOAST`] cell. Nothing GTK-owned is ever reachable from the
/// worker.
static PENDING_TOASTS: Mutex<Vec<Toast>> = Mutex::new(Vec::new());

/// A pending "raise the window" request from another thread.
///
/// Same constraint as [`PENDING_TOASTS`] and the same answer: the
/// closure handed to `MainContext::invoke` must capture nothing, and
/// an activation token plus a card id are payloads. They are parked
/// here and collected by the capture-free [`drain_show_requests`].
///
/// Only the newest survives. These arrive at human-click rate, and
/// two clicks in flight mean the user wants the second one — showing
/// the window twice and scrolling to the older card would be wrong in
/// both halves.
static PENDING_SHOW: Mutex<Option<ShowRequest>> = Mutex::new(None);

/// What a raise request carries beyond "become visible".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ShowRequest {
    /// Request id to scroll into view, when the raise came from a
    /// notification click or a tray entry for one specific request.
    /// `None` for the tray's "Open Vetter…", the footer and
    /// `vet daemon open`, which mean "show the window" and nothing
    /// more specific.
    pub card_id: Option<String>,
    /// XDG activation token from the notification server, if it sent
    /// one. See [`apply_activation_token`].
    pub token: Option<String>,
    /// Picker to raise once the card is on screen (§6i). `Some` only
    /// for the two notification picker actions, and only alongside a
    /// `card_id` — a picker generalises one specific request, so
    /// there is nothing it could mean without one.
    pub picker: Option<PickerKind>,
}

/// A one-line outcome banner: what happened after a picker action.
///
/// The macOS picker surfaces this as a follow-up `NSAlert`. An
/// in-window banner is the better fit here — a modal that appears
/// *after* the work is done has nothing to ask, and stacking a second
/// modal over a still-open picker on Wayland is a good way to lose a
/// window behind its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Toast {
    text: String,
    /// Drives the banner tone. Errors have to be legible as failures;
    /// a rule that silently did not get added is the worst outcome
    /// this surface can produce.
    error: bool,
}

/// Everything the refresh path needs, all of it GTK-thread-owned.
struct WindowState {
    window: ApplicationWindow,
    /// Container the cards are rebuilt into on every refresh.
    list: GtkBox,
    /// The viewport [`list`](Self::list) scrolls inside. Held so a
    /// notification click can point it at one card.
    scroller: ScrolledWindow,
    /// Full daemon context: the pickers call
    /// [`crate::suggestions`] directly rather than round-tripping
    /// through the admin socket, exactly as the macOS popover does.
    ctx: Arc<Context>,
    /// Footer checkbox, re-synced from disk whenever the window is
    /// shown so an external edit to `settings.yaml` is reflected.
    sound: CheckButton,
    autostart: CheckButton,
}

impl WindowState {
    fn queue(&self) -> &Arc<PendingQueue> {
        &self.ctx.pending
    }
}

/// Build the window and park it in the thread-local. Call once, on
/// the GTK thread, before the main loop runs.
pub(super) fn install(app: &Application, ctx: Arc<Context>, shutdown: Arc<AtomicBool>) {
    install_css();

    let list = GtkBox::new(Orientation::Vertical, CARD_SPACING);
    list.set_margin_top(GUTTER);
    list.set_margin_bottom(GUTTER);
    list.set_margin_start(GUTTER);
    list.set_margin_end(GUTTER);

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();

    let (footer, sound, autostart) = footer(Arc::clone(&shutdown));

    // Footer sits outside the scroller so Quit and the settings stay
    // reachable no matter how far down the card list the user is.
    let root = GtkBox::new(Orientation::Vertical, 0);
    root.append(&scroller);
    root.append(&Separator::new(Orientation::Horizontal));
    root.append(&footer);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Vetter")
        .default_width(WINDOW_WIDTH)
        .default_height(WINDOW_HEIGHT)
        .child(&root)
        .build();

    // Closing the window must not quit the daemon: the tray and the
    // notifier are still live surfaces, and `gtk::Application` would
    // otherwise drop its last window and end the main loop, taking
    // the accept loop's driver with it. Hide instead, and let
    // `Quit Vetter` / SIGTERM own shutdown.
    window.connect_close_request(|w| {
        w.set_visible(false);
        glib::Propagation::Stop
    });

    WINDOW.with(|cell| {
        *cell.borrow_mut() = Some(WindowState {
            window,
            list,
            scroller,
            ctx,
            sound,
            autostart,
        });
    });
    WINDOW_INSTALLED.store(true, Ordering::SeqCst);

    sync_sound_checkbox();
    sync_autostart_checkbox();
    refresh();
}

/// The fixed footer strip: settings on the left, Quit on the right.
///
/// Returns the strip plus the sound and autostart checkboxes, which
/// the caller parks in [`WindowState`] so both can be re-synced from
/// disk on show.
fn footer(shutdown: Arc<AtomicBool>) -> (GtkBox, CheckButton, CheckButton) {
    let row = GtkBox::new(Orientation::Horizontal, 12);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    row.set_margin_start(GUTTER);
    row.set_margin_end(GUTTER);

    // Live as of Phase 6e: `autostart` now writes a real XDG entry
    // at `~/.config/autostart/vetter.desktop` (§5.3). Same shape as
    // the sound toggle — the write happens off this thread and the
    // checkbox is re-read from disk on every show, so a user who
    // disables us through their desktop's own autostart UI sees that
    // reflected here rather than fighting the window over it.
    let autostart = CheckButton::with_label("Start at login");
    autostart.set_tooltip_text(Some(
        "Start the Vetter daemon when you log in, by writing an entry \
         to ~/.config/autostart/.",
    ));
    autostart.connect_toggled(|button| set_autostart(button.is_active()));
    row.append(&autostart);

    let sound = CheckButton::with_label("Play sound on new request");
    sound.set_tooltip_text(Some(
        "Ask the notification server to play a sound alongside each \
         approval banner. Whether it does is up to your desktop's \
         notification settings.",
    ));
    sound.connect_toggled(|button| set_notification_sound(button.is_active()));
    row.append(&sound);

    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);

    let quit = Button::with_label("Quit Vetter");
    quit.add_css_class("destructive-action");
    quit.set_tooltip_text(Some(
        "Stop the daemon. Anything still waiting for a decision is denied.",
    ));
    quit.connect_clicked(move |_| {
        // Flip the same flag SIGTERM sets and let the normal shutdown
        // path run — the GLib timeout in `watch_shutdown` notices,
        // destroys the window and quits the application, and
        // `run_with_glib` then denies anything still parked and joins
        // the accept thread. Never `exit()` from a callback: that
        // would skip socket and pidfile cleanup. Same contract as the
        // tray's Quit item.
        shutdown.store(true, Ordering::SeqCst);
    });
    row.append(&quit);

    (row, sound, autostart)
}

/// Re-read `settings.yaml` and set the checkbox without re-entering
/// its own toggle handler.
fn sync_sound_checkbox() {
    let enabled = vetter_core::settings::load()
        .map(|s| s.notification_sound)
        .unwrap_or(true);
    WINDOW.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            if state.sound.is_active() != enabled {
                // `set_active` fires `toggled`, which would write the
                // value straight back to disk. Harmless (it is the
                // value we just read) but pointless IO, and it makes
                // the handler reentrant for no reason — so only touch
                // it when it actually differs.
                state.sound.set_active(enabled);
            }
        }
    });
}

/// Re-read the live autostart state and set the checkbox without
/// re-entering its own toggle handler.
///
/// Reads `autostart::current()` rather than `settings.yaml` because
/// the entry is a file the user can delete or disable from their
/// desktop's own autostart UI. The OS state is the truth; the
/// preference is only what we would converge *to*.
fn sync_autostart_checkbox() {
    let enabled = crate::autostart::current().is_enabled();
    WINDOW.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            if state.autostart.is_active() != enabled {
                state.autostart.set_active(enabled);
            }
        }
    });
}

/// Persist the autostart preference and converge the OS state, off
/// the GTK thread.
///
/// Writes the settings file *first* so a failure to write the entry
/// does not lose the user's stated preference — the next daemon
/// launch retries via `reconcile_with_settings`. Same ordering as
/// `apply_autostart_change` in `lib.rs`, for the same reason.
fn set_autostart(enabled: bool) {
    std::thread::spawn(move || {
        let stored = vetter_core::settings::load().and_then(|mut s| {
            if s.autostart == enabled {
                return Ok(());
            }
            s.autostart = enabled;
            vetter_core::settings::store(&s)
        });
        if let Err(e) = stored {
            push_toast(Toast {
                text: format!("Could not save the autostart setting: {e}"),
                error: true,
            });
            return;
        }
        let applied = if enabled {
            crate::autostart::enable()
        } else {
            crate::autostart::disable()
        };
        match applied {
            Ok(()) => request_refresh(),
            Err(e) => push_toast(Toast {
                text: format!("Could not update the autostart entry: {e}"),
                error: true,
            }),
        }
    });
}

/// Persist the notification-sound preference off the GTK thread.
///
/// A settings write is small, but it is still file IO on the thread
/// painting the UI, and the failure path needs somewhere to go. The
/// checkbox stays where the user put it; a failed write surfaces as
/// an error banner and the next `sync_sound_checkbox` re-reads the
/// truth from disk.
fn set_notification_sound(enabled: bool) {
    std::thread::spawn(move || {
        let result = vetter_core::settings::load().and_then(|mut s| {
            if s.notification_sound == enabled {
                return Ok(());
            }
            s.notification_sound = enabled;
            vetter_core::settings::store(&s)
        });
        if let Err(e) = result {
            push_toast(Toast {
                text: format!("Could not save the sound setting: {e}"),
                error: true,
            });
        }
    });
}

/// Install [`CSS`] once, at the display level so every widget we
/// build picks it up without per-widget providers.
fn install_css() {
    let Some(display) = Display::default() else {
        return;
    };
    let provider = CssProvider::new();
    // `load_from_data`, not `load_from_string`: the latter needs the
    // gtk4 crate's `v4_12` feature, which would raise our minimum GTK
    // above what Debian stable ships and buy nothing here. Without
    // that feature `load_from_data` is not deprecated.
    provider.load_from_data(CSS);
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// The markup palette matching the active theme.
fn palette() -> MarkupPalette {
    let dark = gtk4::Settings::default()
        .map(|s| s.is_gtk_application_prefer_dark_theme())
        .unwrap_or(false);
    if dark {
        DARK_PALETTE
    } else {
        LIGHT_PALETTE
    }
}

/// Rebuild the card list from live queue state. GTK thread only.
///
/// A full rebuild rather than a diff: card counts are small (a
/// handful at most), and the alternative — reconciling widget trees
/// against a new snapshot — is a great deal of machinery for no
/// visible gain. The one thing that must survive a rebuild is
/// disclosure state, which is why it lives in [`EXPANDED`] keyed by
/// request id rather than in the `Expander` widgets this function
/// destroys.
fn refresh() {
    let palette = palette();
    WINDOW.with(|cell| {
        let borrowed = cell.borrow();
        let Some(state) = borrowed.as_ref() else {
            return;
        };

        while let Some(child) = state.list.first_child() {
            state.list.remove(&child);
        }
        // Rebuilt alongside the cards: every widget in the old map
        // has just been removed from the list, so keeping any of it
        // would hand `scroll_to_card` a detached widget whose bounds
        // are meaningless.
        CARD_WIDGETS.with(|cell| cell.borrow_mut().clear());

        // One lock acquisition for both sections, so the two can
        // never disagree about a request that resolved between them.
        let (pending_entries, resolved_entries) = state.ctx.pending.all_entries();
        let pending = card::snapshot(&pending_entries);
        let resolved = resolved::resolved_snapshot(&resolved_entries);
        EXPANDED.with(|e| {
            e.borrow_mut()
                .retain_live(&model::live_ids(&pending, &resolved))
        });

        if let Some(banner) = toast_banner() {
            state.list.append(&banner);
        }

        if pending.is_empty() && resolved.is_empty() {
            state.list.append(&empty_state());
            return;
        }

        if pending.is_empty() {
            // Recent has cards but pending does not. Say so, rather
            // than opening straight onto resolved cards with no
            // action buttons and leaving the user to infer why.
            let note = Label::new(Some(model::NO_PENDING));
            note.add_css_class("dim-label");
            note.set_halign(Align::Center);
            state.list.append(&note);
        }
        for (idx, card) in pending.iter().enumerate() {
            if idx > 0 {
                state.list.append(&Separator::new(Orientation::Horizontal));
            }
            let widget = card_widget(card, state, palette);
            state.list.append(&widget);
            remember_card_widget(&card.id, &widget);
        }

        if !resolved.is_empty() {
            state.list.append(&Separator::new(Orientation::Horizontal));
            let header = Label::new(Some(model::RECENT_TITLE));
            header.set_halign(Align::Start);
            header.add_css_class("dim-label");
            header.add_css_class("caption-heading");
            state.list.append(&header);

            for card in &resolved {
                state.list.append(&Separator::new(Orientation::Horizontal));
                let widget = resolved_widget(card, state, palette);
                state.list.append(&widget);
                // Resolved cards are addressable too: a banner click
                // can arrive after the request was approved from
                // somewhere else, and scrolling to where it went is
                // more useful than ignoring the click.
                remember_card_widget(&card.card.id, &widget);
            }
        }
    });
}

/// Record the widget a card was painted into, so a notification
/// click can find it again.
fn remember_card_widget(id: &str, widget: &gtk4::Widget) {
    CARD_WIDGETS.with(|cell| {
        cell.borrow_mut().insert(id.to_string(), widget.clone());
    });
}

/// The result banner, when there is one to show.
fn toast_banner() -> Option<GtkBox> {
    let toast = TOAST.with(|cell| cell.borrow().clone())?;

    let row = GtkBox::new(Orientation::Horizontal, 8);
    row.add_css_class("vetter-toast");
    row.add_css_class(if toast.error { "error" } else { "ok" });

    let label = Label::new(Some(&toast.text));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.append(&label);

    let dismiss = Button::with_label("Dismiss");
    dismiss.add_css_class("flat");
    dismiss.connect_clicked(|_| {
        TOAST.with(|cell| *cell.borrow_mut() = None);
        request_refresh();
    });
    row.append(&dismiss);
    Some(row)
}

/// Move any results left by background threads onto the GTK thread
/// and repaint. Capture-free so it can be handed to
/// [`glib::MainContext::invoke`] directly.
fn drain_toasts() {
    let drained: Vec<Toast> = {
        let mut g = PENDING_TOASTS.lock().expect("toast queue poisoned");
        std::mem::take(&mut *g)
    };
    // Only the newest survives: these arrive one per user action, and
    // a stack of stale banners would push the cards off-screen.
    if let Some(latest) = drained.into_iter().next_back() {
        TOAST.with(|cell| *cell.borrow_mut() = Some(latest));
    }
    refresh();
}

// ── Activation tokens ───────────────────────────────────────────────────────

// GDK's *setters* for the startup-notification id are per-backend
// (`gdk_wayland_display_set_startup_notification_id`,
// `gdk_x11_display_set_startup_notification_id`); only the getter is
// on the generic `GdkDisplay`. Reaching them from Rust would
// otherwise mean the `gdk4-wayland` **and** `gdk4-x11` crates, which
// also drag in `wayland-client` and the matching pkg-config modules —
// three new dependencies and two new build deps for a focus hint.
//
// Both symbols are already exported from the `libgtk-4` we link, so
// `dlsym` reaches them with no new dependency and covers *both*
// backends through one mechanism, which is what §8's "don't let the
// Wayland-first design regress X11" asks for. Declaring a libc
// function directly follows the precedent in `vet/src/daemon.rs`,
// which declares `setsid` and `kill` the same way.
//
// This degrades at every step: unknown backend, symbol absent (a GTK
// built without that backend), or no token at all, and the window
// still opens — just possibly without focus.
unsafe extern "C" {
    fn dlsym(
        handle: *mut std::ffi::c_void,
        symbol: *const std::ffi::c_char,
    ) -> *mut std::ffi::c_void;
}

/// `RTLD_DEFAULT` on glibc: search every object already loaded into
/// the process, which includes the `libgtk-4` we are linked against.
const RTLD_DEFAULT: *mut std::ffi::c_void = std::ptr::null_mut();

/// Hand the compositor the activation token the notification server
/// gave us, so presenting the window is treated as a continuation of
/// the user's click rather than as an app stealing focus.
///
/// On Wayland a client cannot raise itself unprompted; the token is
/// the whole mechanism (`plans/LinuxApp.md` §5.6). GDK consumes the
/// id on the next present and clears it, so this must be called
/// immediately before [`gtk4::prelude::GtkWindowExt::present`].
///
/// Returns whether the token was actually handed over, for logging —
/// a silent no-op here shows up as "the window opened behind
/// something" much later, which is hard to trace back.
fn apply_activation_token(token: &str) -> bool {
    let Some(display) = Display::default() else {
        return false;
    };
    // The generic `GdkDisplay` has no setter, so pick the backend's
    // by type name. Asking glib rather than guessing from
    // `$WAYLAND_DISPLAY` keeps this honest under XWayland, where both
    // env vars are set but the display is X11.
    let symbol: &[u8] = match display.type_().name() {
        "GdkWaylandDisplay" => b"gdk_wayland_display_set_startup_notification_id\0",
        "GdkX11Display" => b"gdk_x11_display_set_startup_notification_id\0",
        other => {
            eprintln!("vetterd: window: unknown GDK backend `{other}`; not applying token");
            return false;
        }
    };

    let Ok(token) = std::ffi::CString::new(token) else {
        // A token with an interior NUL is malformed; the server sent
        // something we cannot pass through the C boundary.
        return false;
    };

    // SAFETY: `symbol` is a NUL-terminated literal. `dlsym` with
    // `RTLD_DEFAULT` searches loaded objects and returns null when
    // the symbol is absent, which is checked before the call. The
    // resolved function's signature is fixed by GTK's public headers
    // (`GdkDisplay*`, `const char*`), the display pointer is borrowed
    // from a live `Display` that outlives the call, and the string
    // outlives it too — GDK copies both.
    use gtk4::glib::translate::ToGlibPtr;
    unsafe {
        let sym = dlsym(RTLD_DEFAULT, symbol.as_ptr().cast());
        if sym.is_null() {
            // GTK built without this backend. Nothing to do, and not
            // worth a message: the window still opens.
            return false;
        }
        let set: unsafe extern "C" fn(*mut gtk4::gdk::ffi::GdkDisplay, *const std::ffi::c_char) =
            std::mem::transmute(sym);
        set(display.to_glib_none().0, token.as_ptr());
    }
    true
}

/// Queue a result banner from any thread.
fn push_toast(toast: Toast) {
    PENDING_TOASTS
        .lock()
        .expect("toast queue poisoned")
        .push(toast);
    glib::MainContext::default().invoke(drain_toasts);
}

/// Placeholder shown when nothing is pending.
fn empty_state() -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 6);
    column.set_valign(Align::Center);
    column.set_vexpand(true);

    let title = Label::new(Some(model::EMPTY_TITLE));
    title.add_css_class("title-2");
    let body = Label::new(Some(model::EMPTY_BODY));
    body.set_wrap(true);
    body.set_justify(gtk4::Justification::Center);
    body.add_css_class("dim-label");

    column.append(&title);
    column.append(&body);
    column
}

/// One pending approval card, optionally inside the dry-run wrapper.
fn card_widget(card: &CardView, state: &WindowState, palette: MarkupPalette) -> gtk4::Widget {
    let body = card_body(card, state, palette);

    if !card.dry_run {
        return body.upcast();
    }
    // `plans/ApprovalUI.md` "Dry-run wrapper": a titled frame rather
    // than an inline pill, so dry-run and live cards read apart at a
    // glance when both sit in the same list.
    let frame = Frame::builder().label("dry run").child(&body).build();
    frame.add_css_class("vetter-dryrun");
    frame.upcast()
}

/// Shared card chrome: title, URL row, pills.
fn card_head(card: &CardView, badge: Option<&str>) -> GtkBox {
    let frame = GtkBox::new(Orientation::Vertical, 6);
    frame.add_css_class("card");
    frame.set_margin_top(GUTTER);
    frame.set_margin_bottom(GUTTER);
    frame.set_margin_start(GUTTER);
    frame.set_margin_end(GUTTER);

    let title_row = GtkBox::new(Orientation::Horizontal, 6);
    let title = Label::new(Some(&card.title));
    title.set_halign(Align::Start);
    title.add_css_class("heading");
    title_row.append(&title);
    if let Some(badge) = badge {
        // Outcome badge on resolved cards, in place of the action
        // buttons a pending card carries.
        let pill = Label::new(Some(badge));
        pill.add_css_class("vetter-pill");
        pill.add_css_class(if badge == resolved::Outcome::Allowed.label() {
            "positive"
        } else {
            "danger"
        });
        title_row.append(&pill);
    }
    frame.append(&title_row);

    frame.append(&url_row(&card.url));

    if !card.pills.is_empty() {
        frame.append(&pills_row(&card.pills));
    }
    frame
}

fn card_body(card: &CardView, state: &WindowState, palette: MarkupPalette) -> GtkBox {
    let frame = card_head(card, None);

    // Pending cards show their effect rows inline rather than behind
    // a disclosure: someone deciding *right now* should not have to
    // go looking for what the command actually does.
    let rows = card.all_rows();
    for row in &rows {
        frame.append(&effect_row(row));
    }

    frame.append(&raw_disclosure(card, palette));
    frame.append(&Separator::new(Orientation::Horizontal));

    frame.append(&picker_row(card, state));

    let buttons = GtkBox::new(Orientation::Horizontal, 6);
    buttons.set_halign(Align::End);
    // Reject left, Approve right — the same accept/cancel ordering
    // the macOS card uses, so muscle memory carries across.
    buttons.append(&action_button(
        "Reject",
        CardAction::Reject,
        &card.id,
        state.queue(),
        &["destructive-action"],
    ));
    buttons.append(&action_button(
        "Approve",
        CardAction::Approve,
        &card.id,
        state.queue(),
        &["suggested-action"],
    ));
    frame.append(&buttons);

    frame
}

/// One card in the Recent section.
///
/// Differs from a pending card in three ways: an outcome badge
/// instead of Approve/Reject, structured rows behind a "Details"
/// disclosure rather than inline (the decision is already made, so
/// the rows are reference material), and — on auto-allowed cards —
/// the "See approval reason" disclosure carrying the rule
/// attribution and the Revoke button.
fn resolved_widget(
    resolved: &ResolvedCardView,
    state: &WindowState,
    palette: MarkupPalette,
) -> gtk4::Widget {
    let card = &resolved.card;
    let frame = card_head(card, Some(resolved.outcome.label()));

    if !card.rows.is_empty() {
        let rows = GtkBox::new(Orientation::Vertical, 6);
        for row in &card.all_rows() {
            rows.append(&effect_row(row));
        }
        frame.append(&disclosure(
            "Details",
            Disclosure::Details,
            &card.id,
            rows.upcast_ref::<gtk4::Widget>(),
        ));
    }

    frame.append(&raw_disclosure(card, palette));

    if let Some(attribution) = &resolved.attribution {
        frame.append(&approval_reason(card, attribution, state));
    }

    if resolved.show_pickers {
        frame.append(&Separator::new(Orientation::Horizontal));
        frame.append(&picker_row(card, state));
    }

    frame.upcast()
}

/// "See approval reason": why this was auto-allowed, and the offer to
/// take the rule back.
fn approval_reason(
    card: &CardView,
    attribution: &RuleAttribution,
    state: &WindowState,
) -> Expander {
    let body = GtkBox::new(Orientation::Vertical, 6);

    let summary = Label::new(Some(&attribution.summary));
    summary.set_halign(Align::Start);
    summary.set_wrap(true);
    summary.set_xalign(0.0);
    body.append(&summary);

    match attribution.revoke {
        Some(scope) => {
            let revoke = Button::with_label("Revoke rule");
            revoke.add_css_class("destructive-action");
            revoke.set_halign(Align::Start);
            let ctx = Arc::clone(&state.ctx);
            let rule_id = attribution.rule_id.clone();
            let scope_name = attribution.scope.as_str();
            revoke.connect_clicked(move |_| {
                confirm_revoke(Arc::clone(&ctx), rule_id.clone(), scope, scope_name);
            });
            body.append(&revoke);
        }
        None => {
            // Built-in / project / denylist layers are not editable
            // from here. Say why rather than showing a button that
            // would only ever explain itself by failing.
            let note = Label::new(Some(&format!(
                "Rules in the {} layer are not editable from this window. \
                 Use `vet allow rm` for project rules.",
                attribution.scope.as_str()
            )));
            note.set_wrap(true);
            note.set_xalign(0.0);
            note.add_css_class("dim-label");
            body.append(&note);
        }
    }

    disclosure(
        "See approval reason",
        Disclosure::Reason,
        &card.id,
        body.upcast_ref::<gtk4::Widget>(),
    )
}

/// The `Allowlist…` / `Trust host…` row.
///
/// `Trust host…` is gated on the card carrying an `UnknownHost`
/// signal, mirroring the `host_suggestions` engine contract: when the
/// host is already trusted there is nothing to suggest, and a button
/// that opens an empty picker is worse than no button.
fn picker_row(card: &CardView, state: &WindowState) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    row.set_halign(Align::Start);

    let allowlist = Button::with_label("Allowlist…");
    allowlist.add_css_class("flat");
    allowlist.set_tooltip_text(Some(
        "Add a standing rule so requests like this are approved automatically.",
    ));
    let ctx = Arc::clone(&state.ctx);
    let id = card.id.clone();
    allowlist.connect_clicked(move |_| open_allowlist_picker(Arc::clone(&ctx), id.clone()));
    row.append(&allowlist);

    if card.show_trust_host {
        let trust = Button::with_label("Trust host…");
        trust.add_css_class("flat");
        trust.set_tooltip_text(Some(
            "Mark this host as known. Removes the unknown-host warning; \
             does not approve anything on its own.",
        ));
        let ctx = Arc::clone(&state.ctx);
        let id = card.id.clone();
        trust.connect_clicked(move |_| open_host_picker(Arc::clone(&ctx), id.clone()));
        row.append(&trust);
    }
    row
}

/// `[GET] https:// host :8080 /path?query`, one label per token so
/// each can carry its own CSS class without any markup.
fn url_row(url: &UrlView) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 4);
    row.set_halign(Align::Start);

    match url {
        UrlView::Fallback(text) => {
            let label = plain_mono(text);
            label.set_wrap(true);
            row.append(&label);
        }
        UrlView::Http(http) => {
            row.append(&method_label(http));
            row.append(&dim_mono(&http.scheme));
            row.append(&host_pill(&http.host, http.trust));
            if let Some(port) = http.port {
                // A visible port is the "look here" cue that pairs
                // with the non-standard-port pill; quiet ports were
                // already dropped by `cards::url::visible_port`.
                let label = plain_mono(&format!(":{port}"));
                label.add_css_class("warning");
                row.append(&label);
            }
            if !http.tail.text.is_empty() {
                let tail = plain_mono(&http.tail.text);
                tail.set_wrap(true);
                tail.set_hexpand(true);
                tail.set_xalign(0.0);
                if !http.tail.has_path {
                    // Query-only or bare-slash tails read as "this is
                    // a search, not a location" and render muted.
                    tail.add_css_class("dim-label");
                }
                row.append(&tail);
            }
        }
    }
    row
}

fn method_label(http: &HttpUrlView) -> Label {
    let label = plain_mono(&http.method);
    label.add_css_class("vetter-method");
    label.add_css_class(match http.method_tone {
        MethodTone::Read => "read",
        MethodTone::Write => "write",
        MethodTone::Destructive => "destructive",
        MethodTone::Other => "other",
    });
    label
}

/// Host token, tinted by trust class, with the reason on hover.
fn host_pill(host: &str, trust: HostTrust) -> Label {
    let pill = Label::new(Some(host));
    pill.add_css_class("vetter-pill");
    pill.add_css_class(match trust {
        // Loopback is deliberately neutral, not "good": trusted-local
        // is an absence of risk rather than a positive signal.
        HostTrust::Loopback => "neutral",
        HostTrust::Known => "positive",
        HostTrust::Unknown => "warn",
    });
    pill.set_tooltip_text(Some(trust.tooltip()));
    pill
}

/// Signal pills, already deduped and sorted by the model.
fn pills_row(pills: &[PillSpec]) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 4);
    row.set_halign(Align::Start);
    for spec in pills {
        let pill = Label::new(Some(spec.label));
        pill.add_css_class("vetter-pill");
        pill.add_css_class(match spec.tone {
            Tone::Danger => "danger",
            Tone::Warn => "warn",
            Tone::Positive => "positive",
        });
        pill.set_tooltip_text(Some(&spec.tooltip));
        row.append(&pill);
    }
    row
}

/// One effect row: a labelled section with its content beneath.
fn effect_row(row: &EffectRow) -> GtkBox {
    match row {
        EffectRow::Headers(names) => {
            let section = section("headers");
            for name in names {
                let label = plain_mono(name);
                label.add_css_class("accent");
                section.append(&label);
            }
            section
        }
        EffectRow::Body { meta, content } => {
            let section = section("body");
            let meta_label = dim_mono(meta);
            section.append(&meta_label);
            match content {
                BodyContent::Inline(text) => {
                    let label = plain_mono(text);
                    label.set_wrap(true);
                    label.set_selectable(true);
                    section.append(&label);
                }
                BodyContent::FromFile(file) => section.append(&file_row(file)),
                BodyContent::Form(fields) => {
                    for field in fields {
                        let label = plain_mono(field);
                        label.set_wrap(true);
                        section.append(&label);
                    }
                }
            }
            section
        }
        EffectRow::Auth { text, redacted } => {
            let section = section("auth");
            let label = plain_mono(text);
            // Green when the credential never reached the screen:
            // having auth is a good sign, and an alarm colour here
            // reads as "secret leaked" when the opposite is true.
            label.add_css_class(if *redacted { "success" } else { "dim-label" });
            section.append(&label);
            section
        }
        EffectRow::FileRead(file) => {
            let section = section("reads");
            section.append(&file_row(file));
            section
        }
        EffectRow::FileWrite(file) => {
            let section = section("writes");
            section.append(&file_row(file));
            section
        }
        EffectRow::ProcessSpawn(command) => {
            let section = section("spawns");
            let label = plain_mono(command);
            label.set_wrap(true);
            section.append(&label);
            section
        }
    }
}

/// A path plus, when the file actually exists, a button to open it.
fn file_row(file: &FilePath) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.set_halign(Align::Start);

    let label = plain_mono(&file.display);
    label.set_wrap(true);
    label.set_selectable(true);
    row.append(&label);

    if file.can_open {
        let button = Button::with_label("Open file");
        button.add_css_class("flat");
        let path = file.path.clone();
        button.connect_clicked(move |_| open_path(&path));
        row.append(&button);
    }
    row
}

/// Hand a path to the desktop's default handler.
///
/// `xdg-open` is the `NSWorkspace::openURL` analogue (§5.7). Spawned
/// detached and never waited on: it may take arbitrarily long to
/// launch a viewer, and blocking the GTK thread on it would freeze
/// the window. Failure is logged rather than surfaced — the button is
/// already suppressed for paths that do not exist, so the remaining
/// failure modes are "no handler installed", which a dialog could not
/// help with either.
fn open_path(path: &Path) {
    match std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {}
        Err(e) => eprintln!("vetterd: xdg-open {}: {e}", path.display()),
    }
}

/// The "Show raw" disclosure: the §8.5 body plus a copy button.
///
/// This is the safety valve `plans/ApprovalUI.md` describes — when
/// the structured rows have not caught up with a new parser, the raw
/// body still shows exactly what will run. Collapsed by default;
/// whether it is open for *this* card comes from [`EXPANDED`].
fn raw_disclosure(card: &CardView, palette: MarkupPalette) -> Expander {
    let body = Label::new(None);
    // The one `set_markup` in this module. Its argument is built by
    // `spans_to_markup`, which escapes every span's text; see the
    // module docs on text safety.
    body.set_markup(&model::spans_to_markup(&card.raw.spans, palette));
    body.set_halign(Align::Start);
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_selectable(true);
    body.add_css_class("vetter-raw");

    let inner = GtkBox::new(Orientation::Vertical, 4);
    inner.append(&body);

    let copy = Button::with_label("Copy");
    copy.add_css_class("flat");
    copy.set_halign(Align::Start);
    copy.set_tooltip_text(Some("Copy the command to the clipboard"));
    let plain = card.raw.plain.clone();
    copy.connect_clicked(move |button| {
        // The plain (ANSI-stripped) text, so what lands on the
        // clipboard is exactly what the user can see rather than
        // anything the markup layer introduced.
        button.clipboard().set_text(&plain);
    });
    inner.append(&copy);

    disclosure(
        "Show raw",
        Disclosure::Raw,
        &card.id,
        inner.upcast_ref::<gtk4::Widget>(),
    )
}

/// An `Expander` whose open/closed bit is owned by [`EXPANDED`]
/// rather than by the widget.
///
/// The ordering here is load-bearing: `expanded` is set through the
/// builder and the notify handler is connected *afterwards*, so
/// restoring saved state during a rebuild cannot re-enter the handler
/// and write back the value we just read.
fn disclosure(label: &str, kind: Disclosure, id: &str, child: &gtk4::Widget) -> Expander {
    let open = EXPANDED.with(|e| e.borrow().is_open(kind, id));
    let expander = Expander::builder()
        .label(label)
        .child(child)
        .expanded(open)
        .build();

    let id = id.to_string();
    expander.connect_expanded_notify(move |exp| {
        let open = exp.is_expanded();
        EXPANDED.with(|e| e.borrow_mut().set_open(kind, &id, open));
    });
    expander
}

/// A titled vertical group: dim caption, then indented content.
fn section(title: &str) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 2);
    column.set_halign(Align::Fill);
    let caption = Label::new(Some(title));
    caption.set_halign(Align::Start);
    caption.add_css_class("dim-label");
    caption.add_css_class("caption");
    column.append(&caption);
    column
}

/// Monospaced label with no styling of its own. `set_text`, never
/// `set_markup` — see the module docs.
fn plain_mono(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_halign(Align::Start);
    label.set_xalign(0.0);
    label.add_css_class("monospace");
    label
}

fn dim_mono(text: &str) -> Label {
    let label = plain_mono(text);
    label.add_css_class("dim-label");
    label
}

/// A card button wired straight into [`PendingQueue::resolve`].
///
/// The click handler runs on the GTK thread, but `resolve` is
/// thread-safe and its change listener will bounce the refresh back
/// here through [`request_refresh`], so the list repaints by the same
/// path an admin-socket or notification resolve would take. One code
/// path for every resolver is what keeps the surfaces consistent.
fn action_button(
    label: &str,
    action: CardAction,
    id: &str,
    queue: &Arc<PendingQueue>,
    classes: &[&str],
) -> Button {
    let button = Button::with_label(label);
    for class in classes {
        button.add_css_class(class);
    }
    let id = id.to_string();
    let queue = Arc::clone(queue);
    button.connect_clicked(move |_| {
        queue.resolve(&id, action.decision());
    });
    button
}

// ── Pickers ─────────────────────────────────────────────────────────────────

/// Modal sheet width. Wide enough that a moderately verbose YAML
/// preview (one method, one host, a path glob) does not wrap.
const PICKER_WIDTH: i32 = 520;

/// Build a modal sheet over the approval window.
///
/// A plain [`Window`], not an `ApplicationWindow` and not a `Dialog`.
/// Not the latter because `gtk::Dialog` is deprecated in GTK 4.10 and
/// its replacement (`AlertDialog`) needs the gtk4 crate's `v4_10`
/// feature, which would raise our minimum GTK above what Debian
/// stable ships for no gain here. Not the former because
/// `gtk::Application` ends its main loop when its last *application*
/// window closes: a sheet registered with the app would make closing
/// it a coin-flip on whether the daemon's driver survives. A bare
/// `Window` is never added to the application's window list, so
/// closing it is inert.
fn sheet(title: &str) -> (Window, GtkBox) {
    let content = GtkBox::new(Orientation::Vertical, 12);
    content.set_margin_top(GUTTER);
    content.set_margin_bottom(GUTTER);
    content.set_margin_start(GUTTER);
    content.set_margin_end(GUTTER);

    let window = Window::builder()
        .title(title)
        .modal(true)
        .default_width(PICKER_WIDTH)
        .child(&content)
        .build();

    WINDOW.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            window.set_transient_for(Some(&state.window));
        }
    });

    (window, content)
}

/// A vertical radio group over `rows`, returning the buttons so the
/// caller can read the selection back.
///
/// GTK4 has no `RadioButton`: grouped [`CheckButton`]s are the
/// idiom, and joining every button to the first is what makes them
/// mutually exclusive. The preview beneath each radio is set with
/// `set_text` — it is argv-derived and must never be parsed as
/// markup.
fn radio_group(rows: &[PickerRow], default_idx: usize) -> (GtkBox, Vec<CheckButton>) {
    let column = GtkBox::new(Orientation::Vertical, 10);
    let mut buttons: Vec<CheckButton> = Vec::with_capacity(rows.len());

    for (idx, row) in rows.iter().enumerate() {
        let entry = GtkBox::new(Orientation::Vertical, 2);

        let radio = CheckButton::with_label(&row.title);
        if let Some(first) = buttons.first() {
            radio.set_group(Some(first));
        }
        radio.set_active(idx == default_idx);
        entry.append(&radio);

        if !row.preview.is_empty() {
            let preview = plain_mono(&row.preview);
            preview.set_wrap(true);
            preview.set_margin_start(24);
            preview.add_css_class("dim-label");
            entry.append(&preview);
        }

        buttons.push(radio);
        column.append(&entry);
    }
    (column, buttons)
}

/// Index of the active radio, or `None` when somehow none is.
fn selected(buttons: &[CheckButton]) -> Option<usize> {
    buttons.iter().position(|b| b.is_active())
}

/// Cancel / confirm button row, confirm on the right.
fn sheet_buttons(confirm_label: &str, destructive: bool) -> (GtkBox, Button, Button) {
    let row = GtkBox::new(Orientation::Horizontal, 6);
    row.set_halign(Align::End);
    let cancel = Button::with_label("Cancel");
    let confirm = Button::with_label(confirm_label);
    confirm.add_css_class(if destructive {
        "destructive-action"
    } else {
        "suggested-action"
    });
    row.append(&cancel);
    row.append(&confirm);
    (row, cancel, confirm)
}

/// "Allowlist…" — pick a generalisation tier and a duration.
fn open_allowlist_picker(ctx: Arc<Context>, request_id: String) {
    let Some((rules, _hosts, peer_sid)) = suggestions::suggestions_for(&ctx, &request_id) else {
        push_toast(Toast {
            text: "That request is no longer available to build a rule from.".into(),
            error: true,
        });
        return;
    };
    if rules.is_empty() {
        push_toast(Toast {
            text: "No allowlist rule can be suggested for this request.".into(),
            error: true,
        });
        return;
    }

    let (window, content) = sheet("Add to allowlist");

    let blurb = Label::new(Some(
        "Pick a generalisation tier and a duration. The rule is appended to your \
         user allowlist; pending requests it covers are approved immediately.",
    ));
    blurb.set_wrap(true);
    blurb.set_xalign(0.0);
    content.append(&blurb);

    let rows = picker::rule_picker_rows(&rules);
    let (tier_box, tier_radios) = radio_group(&rows, 0);
    content.append(&tier_box);

    let duration_label = Label::new(Some("Duration"));
    duration_label.set_xalign(0.0);
    duration_label.add_css_class("heading");
    content.append(&duration_label);

    let duration_rows: Vec<PickerRow> = DurationChoice::ALL
        .iter()
        .map(|d| PickerRow {
            title: d.title().to_string(),
            preview: String::new(),
        })
        .collect();
    // Default to Forever, preserving the one-click "pick a tier, hit
    // the button" flow for anyone who never touches this group.
    let default_duration = DurationChoice::ALL
        .iter()
        .position(|d| *d == DurationChoice::Forever)
        .unwrap_or(0);
    let (duration_box, duration_radios) = radio_group(&duration_rows, default_duration);
    // "For this terminal session" needs a session id to scope to. When
    // the daemon never resolved one for this connection (headless or
    // non-tty caller) the choice cannot be honoured, so disable it
    // rather than silently writing a rule with no sid — which would
    // behave like Forever.
    if peer_sid.is_none() {
        if let Some(idx) = DurationChoice::ALL
            .iter()
            .position(|d| *d == DurationChoice::ThisSession)
        {
            if let Some(radio) = duration_radios.get(idx) {
                radio.set_sensitive(false);
                radio.set_tooltip_text(Some(
                    "This request has no terminal session recorded, so a \
                     session-scoped rule could not be matched later.",
                ));
            }
        }
    }
    content.append(&duration_box);

    let (buttons, cancel, confirm) = sheet_buttons("Add to user allowlist", false);
    content.append(&buttons);

    let w = window.clone();
    cancel.connect_clicked(move |_| w.close());

    let w = window.clone();
    confirm.connect_clicked(move |_| {
        let Some(tier) = selected(&tier_radios) else {
            return;
        };
        let Some(rule) = rules.get(tier).map(|s| s.rule.clone()) else {
            return;
        };
        let choice = selected(&duration_radios)
            .and_then(|i| DurationChoice::ALL.get(i).copied())
            .unwrap_or(DurationChoice::Forever);
        let rule = picker::rule_with_duration(
            rule,
            choice,
            vetter_core::matcher::now_epoch_secs(),
            peer_sid,
        );
        w.close();
        persist_rule(Arc::clone(&ctx), rule, choice.confirmation_note());
    });

    window.present();
}

/// "Trust host…" — pick a host pattern to mark as known.
fn open_host_picker(ctx: Arc<Context>, request_id: String) {
    let Some((_rules, hosts, _sid)) = suggestions::suggestions_for(&ctx, &request_id) else {
        push_toast(Toast {
            text: "That request is no longer available to build a host pattern from.".into(),
            error: true,
        });
        return;
    };
    if hosts.is_empty() {
        push_toast(Toast {
            text: "This host is already trusted.".into(),
            error: false,
        });
        return;
    }

    let (window, content) = sheet("Trust host");

    let blurb = Label::new(Some(
        "Pick a host pattern. Trusting a host clears the unknown-host warning on \
         current and future cards; it does not approve any request.",
    ));
    blurb.set_wrap(true);
    blurb.set_xalign(0.0);
    content.append(&blurb);

    let rows = picker::host_picker_rows(&hosts);
    let (host_box, radios) = radio_group(&rows, 0);
    content.append(&host_box);

    let (buttons, cancel, confirm) = sheet_buttons("Trust this host", false);
    content.append(&buttons);

    let w = window.clone();
    cancel.connect_clicked(move |_| w.close());

    let w = window.clone();
    confirm.connect_clicked(move |_| {
        let Some(idx) = selected(&radios) else {
            return;
        };
        let Some(entry) = hosts.get(idx).map(|s| s.entry.clone()) else {
            return;
        };
        w.close();
        persist_host(Arc::clone(&ctx), entry);
    });

    window.present();
}

/// Confirm before revoking, then hand off to the background.
///
/// Cancel is the default action: an accidental Return on a sheet the
/// user did not mean to open must not delete a rule.
fn confirm_revoke(ctx: Arc<Context>, rule_id: String, scope: WireScope, scope_name: &'static str) {
    let (window, content) = sheet("Remove rule?");

    let heading = Label::new(Some(&format!("Remove rule `{rule_id}`?")));
    heading.set_xalign(0.0);
    heading.set_wrap(true);
    heading.add_css_class("heading");
    content.append(&heading);

    let blurb = Label::new(Some(&format!(
        "This deletes the rule from your user allowlist. Entries already in \
         Recent stay as a record of what was approved while the rule was \
         active; only future requests are re-prompted.\n\nScope: {scope_name}"
    )));
    blurb.set_wrap(true);
    blurb.set_xalign(0.0);
    content.append(&blurb);

    let (buttons, cancel, confirm) = sheet_buttons("Remove rule", true);
    content.append(&buttons);

    let w = window.clone();
    cancel.connect_clicked(move |_| w.close());
    cancel.grab_focus();

    let w = window.clone();
    confirm.connect_clicked(move |_| {
        w.close();
        revoke_rule(Arc::clone(&ctx), scope, rule_id.clone());
    });

    window.present();
}

// ── Background persistence ──────────────────────────────────────────────────
//
// Every one of these does file IO and reloads an in-memory store, so
// none of it may run on the thread painting the window. The result
// comes back through `push_toast`, which is the capture-free hop
// described at `PENDING_TOASTS`. The queue's own change listener
// repaints the cards, so these threads never need to touch a widget.

/// Persist a new allowlist rule.
fn persist_rule(ctx: Arc<Context>, rule: Rule, duration_note: &'static str) {
    std::thread::spawn(move || {
        let toast = match suggestions::add_allowlist_rule(&ctx, WireScope::User, rule) {
            Ok(added) => {
                let approved = added.auto_approved_ids.len();
                let tail = match approved {
                    0 => String::new(),
                    1 => " Approved 1 waiting request.".into(),
                    n => format!(" Approved {n} waiting requests."),
                };
                Toast {
                    text: format!("Added rule `{}`. {duration_note}{tail}", added.id),
                    error: false,
                }
            }
            Err(e) => Toast {
                text: format!("Could not add the rule: {e}"),
                error: true,
            },
        };
        push_toast(toast);
    });
}

/// Persist a new known-host entry.
fn persist_host(ctx: Arc<Context>, entry: KnownHostEntry) {
    std::thread::spawn(move || {
        let pattern = entry.pattern.clone();
        let toast = match suggestions::add_known_host(&ctx, WireScope::User, entry) {
            Ok(()) => Toast {
                text: format!("Now trusting `{pattern}`."),
                error: false,
            },
            Err(e) => Toast {
                text: format!("Could not trust the host: {e}"),
                error: true,
            },
        };
        push_toast(toast);
    });
}

/// Remove a rule the user revoked.
fn revoke_rule(ctx: Arc<Context>, scope: WireScope, rule_id: String) {
    std::thread::spawn(move || {
        let toast = match suggestions::remove_allowlist_rule(&ctx, scope, &rule_id) {
            Ok(()) => Toast {
                text: format!("Removed rule `{rule_id}`."),
                error: false,
            },
            Err(e) => Toast {
                text: format!("Could not remove the rule: {e}"),
                error: true,
            },
        };
        push_toast(toast);
    });
}

/// Show and focus the window. Safe to call from any thread.
///
/// On Wayland a client cannot raise itself unprompted, so this may
/// surface the window unfocused or merely flag it in the taskbar when
/// the request did not originate from user input.
///
/// A notification body click passes an activation token and the id of
/// the card that was clicked; see [`request_show_for`].
pub(crate) fn request_show() {
    request_show_with(ShowRequest::default());
}

/// Raise the window, scrolled to a card and/or with a picker open.
///
/// Safe to call from any thread. The payload is parked in
/// [`PENDING_SHOW`] rather than captured, because the closure handed
/// to `MainContext::invoke` must capture nothing — see that static.
pub(crate) fn request_show_with(request: ShowRequest) {
    *PENDING_SHOW.lock().expect("show queue poisoned") = Some(request);
    glib::MainContext::default().invoke(drain_show_requests);
}

/// Raise the window scrolled to one request, with no picker.
///
/// The tray's per-request **Open** item (§6i) and anything else that
/// means "show me this one".
pub(crate) fn request_show_card(card_id: String) {
    request_show_with(ShowRequest {
        card_id: Some(card_id),
        ..ShowRequest::default()
    });
}

/// Collect a parked raise request and act on it. Capture-free so it
/// can be handed straight to [`glib::MainContext::invoke`].
fn drain_show_requests() {
    let Some(request) = PENDING_SHOW.lock().expect("show queue poisoned").take() else {
        return;
    };

    // Re-read the preference on the way up: `settings.yaml` can be
    // edited by hand or by a future CLI while the window sits hidden,
    // and a checkbox showing a stale value is worse than one that
    // lags.
    sync_sound_checkbox();
    sync_autostart_checkbox();

    // The token has to be handed over *before* the present it
    // authorises: GDK consumes the id on the next present and clears
    // it. Failure is not fatal — the window opens either way, just
    // possibly without focus (§5.6).
    if let Some(token) = request.token.as_deref() {
        apply_activation_token(token);
    }

    WINDOW.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            state.window.set_visible(true);
            state.window.present();
        }
    });

    if let Some(id) = request.card_id.clone() {
        scroll_to_card(id);
    }

    // A picker asked for from a banner (§6i). Raised after the
    // present so the sheet has a mapped parent to sit over, and after
    // the scroll so the card it refers to is behind it rather than
    // somewhere off-screen. Both picker functions already answer the
    // "that request is gone" case with a toast, so a request resolved
    // between the click and this drain degrades to an explanation
    // rather than an empty sheet.
    if let (Some(kind), Some(id)) = (request.picker, request.card_id) {
        let ctx = WINDOW.with(|cell| cell.borrow().as_ref().map(|s| Arc::clone(&s.ctx)));
        if let Some(ctx) = ctx {
            match kind {
                PickerKind::Allowlist => open_allowlist_picker(ctx, id),
                PickerKind::TrustHost => open_host_picker(ctx, id),
            }
        }
    }
}

/// Scroll the card for `id` into view, if it is on screen.
///
/// Deferred to an idle callback because widget bounds are only
/// meaningful once GTK has allocated them, and the present above may
/// have just made the window visible for the first time.
///
/// A miss is silent and correct: the request may have been resolved
/// by someone else between the click and its delivery, in which case
/// the card has moved to Recent or fallen out of the ring entirely.
/// The window still opens — which is the useful half of the click —
/// and scrolling to a card that no longer exists is not a thing to
/// report.
fn scroll_to_card(id: String) {
    glib::idle_add_local_once(move || {
        let Some(widget) = CARD_WIDGETS.with(|cell| cell.borrow().get(&id).cloned()) else {
            return;
        };
        WINDOW.with(|cell| {
            let borrowed = cell.borrow();
            let Some(state) = borrowed.as_ref() else {
                return;
            };
            // Bounds are relative to the list the cards are packed
            // into, which is exactly the scroller's coordinate space.
            let Some(bounds) = widget.compute_bounds(&state.list) else {
                return;
            };
            let adjustment = state.scroller.vadjustment();
            // Clamp rather than trusting the bounds: an unallocated
            // widget can report a y beyond the scrollable range, and
            // GTK would otherwise snap to the bottom.
            let target = f64::from(bounds.y())
                .min(adjustment.upper() - adjustment.page_size())
                .max(adjustment.lower());
            adjustment.set_value(target);
        });
    });
}

/// Repaint from live queue state. Safe to call from any thread; this
/// is what the queue's change listener calls.
pub(crate) fn request_refresh() {
    glib::MainContext::default().invoke(refresh);
}

/// Poll the shutdown flag and quit the main loop when it flips.
///
/// The AppKit path dispatches `NSApplication::terminate` from the
/// signal thread; GTK has no equivalent that is safe to call from a
/// signal handler, so we poll on the same 100 ms cadence the accept
/// loop uses. Worst case the window lives 100 ms past SIGTERM, well
/// inside the ~250 ms §7 step 15 allows for the tray to vanish.
pub(super) fn watch_shutdown(app: &Application, shutdown: Arc<AtomicBool>) {
    let app = app.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if shutdown.load(Ordering::SeqCst) {
            // Drop the window before quitting so its widgets are
            // destroyed on this thread rather than at process teardown.
            WINDOW_INSTALLED.store(false, Ordering::SeqCst);
            WINDOW.with(|cell| {
                if let Some(state) = cell.borrow_mut().take() {
                    state.window.destroy();
                }
            });
            CARD_WIDGETS.with(|cell| cell.borrow_mut().clear());
            app.quit();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}
