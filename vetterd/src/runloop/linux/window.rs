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

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, Label, Orientation, PolicyType,
    ScrolledWindow, Separator,
};

use super::model::{self, CardAction, CardView};
use crate::cards::url::HostTrust;
use crate::pending::PendingQueue;

/// Window chrome sizing. Wide enough for a realistic URL without
/// wrapping, tall enough for three cards before scrolling.
const WINDOW_WIDTH: i32 = 520;
const WINDOW_HEIGHT: i32 = 460;
const GUTTER: i32 = 12;

// GTK-thread-only handle to the live window.
//
// A thread-local rather than a global is the whole safety argument
// (see the module docs): a non-GTK thread that somehow ran this code
// would observe `None` rather than a widget it must not touch.
thread_local! {
    static WINDOW: RefCell<Option<WindowState>> = const { RefCell::new(None) };
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
    let list = GtkBox::new(Orientation::Vertical, GUTTER);
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

/// Rebuild the card list from live queue state. GTK thread only.
fn refresh() {
    WINDOW.with(|cell| {
        let borrowed = cell.borrow();
        let Some(state) = borrowed.as_ref() else {
            return;
        };

        while let Some(child) = state.list.first_child() {
            state.list.remove(&child);
        }

        let cards = model::snapshot(&state.queue.pending_summaries());
        if cards.is_empty() {
            state.list.append(&empty_state());
        } else {
            for card in cards {
                state.list.append(&card_widget(&card, &state.queue));
            }
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

/// One approval card: title, target, host-trust pill, and the two
/// buttons.
///
/// NOTE for step 2: the §8.5 detail disclosure, the raw-command
/// disclosure with copy, the signal pills and the per-effect rows
/// (including `Open file` via `xdg-open`) attach between the target
/// row and the button row. `crate::cards` already lowers all of them.
///
/// NOTE for step 3: `Allowlist…` / `Trust host…` and, on auto-allowed
/// Recent cards, `See approval reason` / `Revoke rule` join the
/// button row.
fn card_widget(card: &CardView, queue: &Arc<PendingQueue>) -> GtkBox {
    let frame = GtkBox::new(Orientation::Vertical, 6);
    frame.add_css_class("card");
    frame.set_margin_bottom(2);

    let title = Label::new(Some(&card.title));
    title.set_halign(Align::Start);
    title.add_css_class("heading");
    frame.append(&title);

    let target = Label::new(Some(&card.target));
    target.set_halign(Align::Start);
    target.set_wrap(true);
    target.set_selectable(true);
    target.add_css_class("monospace");
    frame.append(&target);

    frame.append(&trust_pill(card.trust));
    frame.append(&Separator::new(Orientation::Horizontal));

    let buttons = GtkBox::new(Orientation::Horizontal, 6);
    buttons.set_halign(Align::End);
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

/// Host-trust indicator. Colour comes from GTK's own semantic
/// classes rather than hard-coded hex so the pill tracks the user's
/// theme, light or dark.
fn trust_pill(trust: HostTrust) -> Label {
    let (text, class) = match trust {
        HostTrust::Loopback => ("loopback", "accent"),
        HostTrust::Known => ("known host", "success"),
        HostTrust::Unknown => ("unknown host", "warning"),
    };
    let pill = Label::new(Some(text));
    pill.set_halign(Align::Start);
    pill.set_tooltip_text(Some(trust.tooltip()));
    pill.add_css_class(class);
    pill.add_css_class("caption");
    pill
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
