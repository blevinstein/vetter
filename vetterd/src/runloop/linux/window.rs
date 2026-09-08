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
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk4::gdk::Display;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, Expander, Frame,
    Label, Orientation, PolicyType, ScrolledWindow, Separator,
};

use super::model::{
    self, BodyContent, CardAction, CardView, EffectRow, FilePath, HttpUrlView, MarkupPalette,
    UrlView,
};
use crate::cards::pills::{PillSpec, Tone};
use crate::cards::url::{HostTrust, MethodTone};
use crate::pending::PendingQueue;

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
}

/// Everything the refresh path needs, all of it GTK-thread-owned.
struct WindowState {
    window: ApplicationWindow,
    /// Container the cards are rebuilt into on every refresh.
    list: GtkBox,
    queue: Arc<PendingQueue>,
}

/// Build the window and park it in the thread-local. Call once, on
/// the GTK thread, before the main loop runs.
pub(super) fn install(app: &Application, queue: Arc<PendingQueue>) {
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

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Vetter")
        .default_width(WINDOW_WIDTH)
        .default_height(WINDOW_HEIGHT)
        .child(&scroller)
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
            queue,
        });
    });

    refresh();
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

        let cards = model::snapshot(&state.queue.pending_entries());
        EXPANDED.with(|e| e.borrow_mut().retain_live(&cards));

        if cards.is_empty() {
            state.list.append(&empty_state());
            return;
        }
        for (idx, card) in cards.iter().enumerate() {
            if idx > 0 {
                state.list.append(&Separator::new(Orientation::Horizontal));
            }
            let raw_open = EXPANDED.with(|e| e.borrow().is_raw_open(&card.id));
            state
                .list
                .append(&card_widget(card, &state.queue, raw_open, palette));
        }
    });
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

/// One approval card, optionally inside the dry-run wrapper.
///
/// NOTE for step 3: `Allowlist…` / `Trust host…` join the button row
/// below, and `See approval reason` / `Revoke rule` arrive with the
/// Recent (resolved) section, which this window does not yet show.
/// When Recent lands, its structured rows belong behind a "▸ Details"
/// disclosure — `EXPANDED` already has room for a second kind — while
/// pending cards keep them inline as they are here, because a user
/// deciding *now* should not have to go looking.
fn card_widget(
    card: &CardView,
    queue: &Arc<PendingQueue>,
    raw_open: bool,
    palette: MarkupPalette,
) -> gtk4::Widget {
    let body = card_body(card, queue, raw_open, palette);

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

fn card_body(
    card: &CardView,
    queue: &Arc<PendingQueue>,
    raw_open: bool,
    palette: MarkupPalette,
) -> GtkBox {
    let frame = GtkBox::new(Orientation::Vertical, 6);
    frame.add_css_class("card");
    frame.set_margin_top(GUTTER);
    frame.set_margin_bottom(GUTTER);
    frame.set_margin_start(GUTTER);
    frame.set_margin_end(GUTTER);

    let title = Label::new(Some(&card.title));
    title.set_halign(Align::Start);
    title.add_css_class("heading");
    frame.append(&title);

    frame.append(&url_row(&card.url));

    if !card.pills.is_empty() {
        frame.append(&pills_row(&card.pills));
    }

    for row in &card.rows {
        frame.append(&effect_row(row));
    }

    frame.append(&raw_disclosure(card, raw_open, palette));
    frame.append(&Separator::new(Orientation::Horizontal));

    let buttons = GtkBox::new(Orientation::Horizontal, 6);
    buttons.set_halign(Align::End);
    // Reject left, Approve right — the same accept/cancel ordering
    // the macOS card uses, so muscle memory carries across.
    buttons.append(&action_button(
        "Reject",
        CardAction::Reject,
        &card.id,
        queue,
        &["destructive-action"],
    ));
    buttons.append(&action_button(
        "Approve",
        CardAction::Approve,
        &card.id,
        queue,
        &["suggested-action"],
    ));
    frame.append(&buttons);

    frame
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
fn raw_disclosure(card: &CardView, open: bool, palette: MarkupPalette) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 4);

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

    let expander = Expander::builder()
        .label("Show raw")
        .child(&inner)
        .expanded(open)
        .build();

    // Connect *after* setting `expanded`, so restoring saved state
    // during a rebuild does not re-enter the handler and write back
    // the value we just read.
    let id = card.id.clone();
    expander.connect_expanded_notify(move |exp| {
        let open = exp.is_expanded();
        EXPANDED.with(|e| e.borrow_mut().set_raw_open(&id, open));
    });

    column.append(&expander);
    column
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

/// Show and focus the window. Safe to call from any thread.
///
/// On Wayland a client cannot raise itself unprompted, so this may
/// surface the window unfocused or merely flag it in the taskbar when
/// the request did not originate from user input.
///
/// NOTE for step 4: the notification click-through passes an XDG
/// activation token (`ActivationToken` / the `activation-token`
/// hint), which is what lets the compositor grant focus. It also
/// wants to scroll the matching card into view.
pub(crate) fn request_show() {
    glib::MainContext::default().invoke(|| {
        WINDOW.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                state.window.set_visible(true);
                state.window.present();
            }
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
            WINDOW.with(|cell| {
                if let Some(state) = cell.borrow_mut().take() {
                    state.window.destroy();
                }
            });
            app.quit();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}
