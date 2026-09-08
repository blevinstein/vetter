//! Real Linux [`Notifier`] backed by the freedesktop notification
//! spec (`org.freedesktop.Notifications`).
//!
//! The macOS counterpart ([`super::mac`]) is a thin handle that
//! marshals work onto the AppKit main thread. Linux has no such
//! thread to borrow, so this module owns its own: the session-bus
//! connection is driven entirely from threads we spawn in
//! [`LinuxNotifier::install`], and the daemon's main thread keeps
//! running the accept loop ([`crate::PlatformDriver::None`]).
//!
//! ## Threads
//!
//! Three, all spawned at install time and all detached:
//!
//! - **jobs** — owns every *outgoing* bus call (`Notify`,
//!   `CloseNotification`). Fed by an `mpsc` channel so no caller ever
//!   blocks on the bus. This matters: [`Notifier::notify`] runs on a
//!   connection worker, and the queue's change listener runs on
//!   whatever thread resolved a request (a bus thread, or the admin
//!   socket's single-threaded handler). A stalled bus must not wedge
//!   either of those. `plans/LinuxApp.md` §11.
//! - **signals** — iterates every signal on the Notifications
//!   interface and dispatches `ActionInvoked` / `NotificationClosed`
//!   / `ActivationToken` by member name.
//! - **names** — watches `NameOwnerChanged` for
//!   `org.freedesktop.Notifications` so we re-query `GetCapabilities`
//!   when the notification daemon restarts, and drop the id map it
//!   invalidated (§5.4, §11).
//!
//! Two signal threads rather than one because the blocking API's
//! iterators can only be waited on one at a time; selecting across
//! both in a single thread would mean pulling in an async runtime,
//! which the dependency budget for this phase rules out.
//!
//! ## Banner lifetime
//!
//! A prompt-class request must never silently vanish, so every
//! `Notify` goes out with `expire_timeout = 0` (never expire) and
//! `urgency = critical`. The flip side is that nothing reaps those
//! banners for us: we close them ourselves from the queue's change
//! listener, which fires for *every* resolve path — a notification
//! action, `vet daemon approve` over the admin socket (Phase 6a), an
//! `AddRule` that auto-approved the entry, or shutdown's
//! `cancel_all`. That is why the reconciliation is driven off queue
//! state rather than off the code path that happened to resolve it.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};
use zbus::zvariant::Value;

use crate::cards::markup::escape_markup as escape_body_markup;
use crate::pending::{NotifyHint, PendingDecision, PendingQueue, PromptSummary};

use super::{Notifier, NotifierBuildError};

/// Well-known name, object path, and interface of the notification
/// server. All three are fixed by the freedesktop spec.
const NOTIFY_NAME: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";

/// `app_name` passed to `Notify`. Shown by some servers next to the
/// body; Plasma prefers the `desktop-entry` hint below.
const APP_NAME: &str = "Vetter";

/// Basename (no `.desktop` suffix) of the entry that gives our
/// banners a name and an icon instead of a generic placeholder.
/// Purely cosmetic — unlike the macOS bundle check this is **not** a
/// precondition for starting (`plans/LinuxApp.md` §5.2). The entry
/// itself ships in Phase 6c; until then servers fall back to
/// `app_name` and a default icon.
const DESKTOP_ENTRY: &str = "dev.vetter.daemon";

/// Action keys we register on every banner. The spec wants
/// `[key, label, key, label, ...]`; the key is what comes back on
/// `ActionInvoked`.
const ACTION_APPROVE: &str = "approve";
const ACTION_REJECT: &str = "reject";

/// The spec's reserved key for "the user clicked the banner body".
///
/// Servers only deliver it if the key is *registered* in the actions
/// array like any other, even though most — Plasma included — render
/// no button for it. The label is therefore rarely seen, but it is
/// what a server that does render one would show.
const ACTION_DEFAULT: &str = "default";

/// `sound-name` hint value. An XDG sound-naming-spec name rather than
/// a file path, so the server picks the theme's own sound.
const SOUND_NAME: &str = "dialog-question";

/// Audit reasons. Same shape as Phase 6a's `… via admin socket`, and
/// the strings `plans/LinuxApp.md` §7 step 13 expects to see in the
/// log.
const REASON_APPROVED: &str = "approved via notification";
const REASON_REJECTED: &str = "rejected via notification";

// ── Pure helpers (unit-tested without a bus) ────────────────────────────────

/// Capabilities we care about, projected out of the server's
/// `GetCapabilities` string list.
///
/// Deliberately a struct of bools rather than the raw `Vec<String>`:
/// the call is cached and re-queried on server restart, and every
/// consumer wants a specific question answered, not a list to scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// Server renders action buttons. When absent our Approve /
    /// Reject buttons would simply not appear, so we say so on
    /// stderr and lean on `vet daemon approve` instead (§5.4).
    pub actions: bool,
    /// Server parses a small HTML subset in the body. When present
    /// the body **must** be escaped — an argv-derived URL containing
    /// `<b>` would otherwise render as markup and let a hostile
    /// command restyle the text a human is about to approve.
    pub body_markup: bool,
}

impl Capabilities {
    pub(crate) fn from_list<S: AsRef<str>>(caps: &[S]) -> Self {
        let has = |want: &str| caps.iter().any(|c| c.as_ref() == want);
        Self {
            actions: has("actions"),
            body_markup: has("body-markup"),
        }
    }
}

/// Map an `ActionInvoked` key onto the decision it stands for.
/// `None` for anything that is not one of the two decision buttons —
/// including the spec's `"default"` key, which is the *body* click
/// and never resolves anything (see [`ActionIntent`]).
pub(crate) fn decision_for_action(key: &str) -> Option<PendingDecision> {
    match key {
        ACTION_APPROVE => Some(PendingDecision::allow(REASON_APPROVED)),
        ACTION_REJECT => Some(PendingDecision::deny(REASON_REJECTED)),
        _ => None,
    }
}

/// What the signal thread should do about an `ActionInvoked` key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActionIntent {
    /// One of the two buttons: resolve the request.
    Resolve(PendingDecision),
    /// The banner body was clicked. Open the approval window on that
    /// card and leave the request **pending**.
    ///
    /// This distinction is the whole safety property of the body
    /// click: a notification sitting on screen is easy to brush past,
    /// and a stray click that silently approved a command would be
    /// the worst failure this surface could have. Opening a window
    /// costs the user a moment; approving costs them the decision.
    OpenWindow,
    /// A key we do not recognise. Servers are free to invent them,
    /// and a future action added here would arrive at old daemons.
    Ignore,
}

/// Route an `ActionInvoked` key.
///
/// Split from [`decision_for_action`] so the "never resolves" half of
/// the body-click contract is stated once and testable on its own.
pub(crate) fn action_intent(key: &str) -> ActionIntent {
    if key == ACTION_DEFAULT {
        return ActionIntent::OpenWindow;
    }
    match decision_for_action(key) {
        Some(decision) => ActionIntent::Resolve(decision),
        None => ActionIntent::Ignore,
    }
}

/// What the signal thread does with one `ActionInvoked`, given the
/// key and whichever request the notification id still maps to.
///
/// Pure so the awkward case is testable without a bus: the id may map
/// to nothing by the time the click arrives. A banner outlives the
/// request behind it whenever someone resolved it from another
/// surface first — the window, `vet daemon approve`, a rule that
/// auto-approved it — and the click is then racing a close that is
/// already queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClickOutcome {
    /// Resolve `request`. Only a button can produce this.
    Resolve {
        request: String,
        decision: PendingDecision,
    },
    /// Raise the window, scrolled to `card` when there is still one
    /// to scroll to.
    Show { card: Option<String> },
    /// Nothing to do.
    Nothing,
}

pub(crate) fn click_outcome(key: &str, request: Option<&str>) -> ClickOutcome {
    match (action_intent(key), request) {
        (ActionIntent::Ignore, _) => ClickOutcome::Nothing,
        // A button click on a banner we no longer have a request for
        // is dropped rather than guessed at: resolving the wrong
        // request would be an unrequested decision on somebody's
        // command.
        (ActionIntent::Resolve(_), None) => ClickOutcome::Nothing,
        (ActionIntent::Resolve(decision), Some(request)) => ClickOutcome::Resolve {
            request: request.to_string(),
            decision,
        },
        // A body click always opens the window, even with no request
        // to point at: the user asked to see Vetter, and the card
        // list is the honest answer to that.
        (ActionIntent::OpenWindow, card) => ClickOutcome::Show {
            card: card.map(str::to_string),
        },
    }
}

/// Coalescing predicate, mirroring [`super::mac::MacNotifier::notify`].
/// Spec §7: "if multiple requests are queued, notifications coalesce
/// after the first; we don't spam banners". `was_empty_before` is
/// captured inside the queue's submit critical section, so exactly
/// one banner is raised per burst even under concurrent submits.
pub(crate) fn should_raise_banner(hint: NotifyHint) -> bool {
    hint.was_empty_before
}

/// `sound-name` hint for the user's `notification_sound` preference.
pub(crate) fn sound_name_for(play_sound: bool) -> Option<&'static str> {
    play_sound.then_some(SOUND_NAME)
}

/// Build the `(summary, body)` pair for a banner.
///
/// Every argv-derived string goes through
/// [`vetter_core::render::sanitize_for_display`] first: a hostile URL
/// or path carrying RTLO / zero-width / control bytes would otherwise
/// be painted verbatim by the notification server, hiding the real
/// target from the human about to click **Approve**. Same defence
/// the macOS banner applies in `runloop::post_notification`.
pub(crate) fn banner_text(summary: &PromptSummary, caps: Capabilities) -> (String, String) {
    use vetter_core::render::sanitize_for_display;
    let command = sanitize_for_display(&summary.command);
    let verb = sanitize_for_display(&summary.primary_verb);
    let target = sanitize_for_display(&summary.primary_target);

    // macOS puts "dry run" in the subtitle; the freedesktop spec has
    // no subtitle field, so it rides on the title instead.
    let title = if summary.force_prompt {
        format!("vet {command} (dry run)")
    } else {
        format!("vet {command}")
    };
    let body = if summary.primary_verb.is_empty() {
        target.into_owned()
    } else {
        format!("{verb} {target}")
    };
    let body = if caps.body_markup {
        escape_body_markup(&body)
    } else {
        body
    };
    (title, body)
}

/// Bidirectional notification-id ↔ request-ULID map.
///
/// Notification ids are `u32`s assigned by the *server* and reused
/// freely across server restarts, so they are never a primary key for
/// anything of ours (`plans/LinuxApp.md` §11). Linking is therefore
/// eviction-safe in both directions: re-linking either side drops the
/// stale pairing rather than leaving a half-entry that would later
/// close somebody else's banner.
#[derive(Debug, Default)]
pub(crate) struct NotificationMap {
    by_notification: HashMap<u32, String>,
    by_request: HashMap<String, u32>,
}

impl NotificationMap {
    pub(crate) fn link(&mut self, notification: u32, request: &str) {
        if let Some(stale) = self
            .by_notification
            .insert(notification, request.to_string())
        {
            self.by_request.remove(&stale);
        }
        if let Some(stale) = self.by_request.insert(request.to_string(), notification) {
            if stale != notification {
                self.by_notification.remove(&stale);
            }
        }
    }

    pub(crate) fn request_for(&self, notification: u32) -> Option<&str> {
        self.by_notification.get(&notification).map(String::as_str)
    }

    /// Forget the pairing for `notification`, returning the request
    /// it stood for. Used on `NotificationClosed`, where the server
    /// tells us the banner is already gone.
    pub(crate) fn forget_notification(&mut self, notification: u32) -> Option<String> {
        let request = self.by_notification.remove(&notification)?;
        self.by_request.remove(&request);
        Some(request)
    }

    /// Forget the pairing for `request`, returning the notification
    /// id that needs closing.
    pub(crate) fn forget_request(&mut self, request: &str) -> Option<u32> {
        let notification = self.by_request.remove(request)?;
        self.by_notification.remove(&notification);
        Some(notification)
    }

    /// Every request id we currently hold a banner for.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.by_request.keys().cloned().collect()
    }

    /// Drop everything. Called when the notification server leaves
    /// the bus: its ids are meaningless to its replacement.
    pub(crate) fn clear(&mut self) {
        self.by_notification.clear();
        self.by_request.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.by_request.len()
    }
}

/// Given the requests we hold banners for and the requests still
/// pending, return the ones whose banner should be closed.
///
/// Pure so the reconciliation rule is testable without a bus. Split
/// out from the worker because this — not the code path that
/// happened to call `resolve` — is what makes "close the banner on
/// *every* resolve path" true.
pub(crate) fn banners_to_close(held: &[String], still_pending: &[String]) -> Vec<String> {
    held.iter()
        .filter(|id| !still_pending.iter().any(|p| p == *id))
        .cloned()
        .collect()
}

// ── Bus plumbing ────────────────────────────────────────────────────────────

/// Work handed to the jobs thread. Everything that talks *out* to the
/// bus goes through here so no other thread ever blocks on it.
enum Job {
    /// Post a banner for a pending request.
    Post(Box<PromptSummary>),
    /// Re-derive which banners are stale and close them.
    Reconcile,
    /// Re-read `GetCapabilities` after a server restart.
    RefreshCapabilities,
    /// Drain and exit.
    Stop,
}

/// State shared between the notifier handle and its threads.
struct Shared {
    queue: Arc<PendingQueue>,
    conn: Connection,
    map: Mutex<NotificationMap>,
    caps: Mutex<Capabilities>,
    /// XDG activation tokens the server has handed us, keyed by
    /// notification id.
    ///
    /// Plasma emits `ActivationToken` immediately *before* the
    /// `ActionInvoked` it belongs to, so the token has to be parked
    /// somewhere for the few microseconds between them (§5.6).
    ///
    /// Bounded by [`prune_tokens`]: a server may emit a token for an
    /// interaction that never produces an `ActionInvoked` — a
    /// long-press, a hover on some shells — and those would otherwise
    /// accumulate for the daemon's lifetime.
    tokens: Mutex<HashMap<u32, String>>,
    shutdown: AtomicBool,
}

impl Shared {
    fn proxy(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(&self.conn, NOTIFY_NAME, NOTIFY_PATH, NOTIFY_NAME)
    }

    fn capabilities(&self) -> Capabilities {
        *self.caps.lock().expect("caps mutex poisoned")
    }

    /// Query `GetCapabilities` and cache the result. Non-fatal on
    /// error: the notification server can legitimately be absent at
    /// daemon start and appear later, which is exactly what the
    /// `NameOwnerChanged` watch exists to catch (§5.4).
    fn refresh_capabilities(&self) {
        let caps = match self.proxy().and_then(|p| p.call("GetCapabilities", &())) {
            Ok(list) => {
                let list: Vec<String> = list;
                Capabilities::from_list(&list)
            }
            Err(e) => {
                eprintln!("vetterd: notifications: GetCapabilities failed: {e}");
                Capabilities::default()
            }
        };
        if !caps.actions {
            // Not fatal, but the operator should know why no buttons
            // appeared. Phase 6d adds the click-through-to-window
            // fallback the plan's §5.4 describes; until then the
            // documented escape hatch is the admin socket.
            eprintln!(
                "vetterd: notifications: server does not advertise `actions`; \
                 banners will have no Approve/Reject buttons — resolve with \
                 `vet daemon approve <id>` / `vet daemon reject <id>`"
            );
        }
        *self.caps.lock().expect("caps mutex poisoned") = caps;
    }

    /// Post one banner and record the id the server assigned.
    fn post(&self, summary: &PromptSummary) {
        let caps = self.capabilities();
        let (title, body) = banner_text(summary, caps);

        // Only offer buttons the server will actually render. Sending
        // actions to a server without the capability is harmless but
        // misleading in a bus trace.
        // `default` is registered first because the spec's ordering
        // is positional for *rendered* buttons and servers skip the
        // reserved key when laying them out — Approve stays the
        // leftmost visible button. Registering it at all is what
        // makes a body click arrive as `ActionInvoked`; without it
        // the click is swallowed by the server.
        let actions: Vec<&str> = if caps.actions {
            vec![
                ACTION_DEFAULT,
                "Open Vetter",
                ACTION_APPROVE,
                "Approve",
                ACTION_REJECT,
                "Reject",
            ]
        } else {
            Vec::new()
        };

        // Re-read the preference on every banner rather than caching
        // it: this fires at human-approval rate, not in a hot path,
        // and staying stateless means a settings rewrite takes effect
        // without anyone having to invalidate a cache. Same reasoning
        // as the macOS notifier.
        let play_sound = vetter_core::settings::load()
            .unwrap_or_default()
            .notification_sound;

        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        // 2 = Critical. Together with `expire_timeout = 0` this is
        // what stops a prompt from silently ageing out of the
        // server's queue (§11).
        hints.insert("urgency", Value::U8(2));
        hints.insert("desktop-entry", Value::Str(DESKTOP_ENTRY.into()));
        hints.insert("category", Value::Str("device".into()));
        if let Some(sound) = sound_name_for(play_sound) {
            hints.insert("sound-name", Value::Str(sound.into()));
        }

        let args = (
            APP_NAME,
            0u32, // replaces_id: never replace; coalescing is upstream
            "",   // app_icon: comes from the desktop entry
            title.as_str(),
            body.as_str(),
            actions,
            hints,
            0i32, // expire_timeout: 0 = never expire
        );
        match self
            .proxy()
            .and_then(|p| p.call::<_, _, u32>("Notify", &args))
        {
            Ok(notification) => {
                self.map
                    .lock()
                    .expect("notification map poisoned")
                    .link(notification, &summary.id);
            }
            Err(e) => {
                eprintln!(
                    "vetterd: notifications: Notify failed for id `{}`: {e}",
                    summary.id
                );
            }
        }
    }

    /// Close the banners of every request that is no longer pending.
    fn reconcile(&self) {
        let held = self
            .map
            .lock()
            .expect("notification map poisoned")
            .requests();
        if held.is_empty() {
            return;
        }
        let still_pending: Vec<String> = self
            .queue
            .pending_summaries()
            .into_iter()
            .map(|s| s.id)
            .collect();
        for request in banners_to_close(&held, &still_pending) {
            let notification = self
                .map
                .lock()
                .expect("notification map poisoned")
                .forget_request(&request);
            let Some(notification) = notification else {
                continue;
            };
            if let Err(e) = self
                .proxy()
                .and_then(|p| p.call::<_, _, ()>("CloseNotification", &(notification,)))
            {
                // Not worth escalating: the usual cause is that the
                // user already dismissed the banner, which the server
                // reports as an error on a now-unknown id.
                eprintln!(
                    "vetterd: notifications: CloseNotification({notification}) \
                     for id `{request}`: {e}"
                );
            }
        }
    }
}

/// Handle stored on the daemon's [`crate::Context`].
pub struct LinuxNotifier {
    shared: Arc<Shared>,
    /// `mpsc::Sender` is `Send` but not `Sync`, and [`Notifier`]
    /// requires both. The mutex is uncontended in practice — one
    /// `send` per prompt and per queue change.
    jobs: Mutex<Sender<Job>>,
}

impl LinuxNotifier {
    /// Connect to the session bus and bring the notification surface
    /// up.
    ///
    /// The precondition we guard on is a **reachable session bus**,
    /// not any notion of an installed application (`plans/LinuxApp.md`
    /// §5.2): Linux has no `.app`-bundle analogue, and the `.desktop`
    /// entry that gives banners their name and icon is cosmetic. A
    /// missing *notification server* is likewise not fatal — it can
    /// join the bus later and `NameOwnerChanged` will pick it up.
    /// What is fatal is having no bus at all, because then nothing
    /// this notifier does can ever reach a human and every
    /// prompt-class request would park forever.
    pub fn install(queue: Arc<PendingQueue>) -> Result<Self, NotifierBuildError> {
        let conn = Connection::session().map_err(|e| {
            NotifierBuildError::Setup(format!(
                "VETTERD_NOTIFIER=linux requires a reachable session bus \
                 (D-Bus): {e}. Start the daemon inside a desktop session, or \
                 set `VETTERD_NOTIFIER=noop` to opt out of the notification \
                 surface (every prompt-class request will then park until \
                 resolved with `vet daemon approve <id>`)."
            ))
        })?;

        let shared = Arc::new(Shared {
            queue,
            conn,
            map: Mutex::new(NotificationMap::default()),
            caps: Mutex::new(Capabilities::default()),
            tokens: Mutex::new(HashMap::new()),
            shutdown: AtomicBool::new(false),
        });
        shared.refresh_capabilities();

        let (tx, rx) = channel::<Job>();
        spawn_jobs_thread(Arc::clone(&shared), rx);
        spawn_signal_thread(Arc::clone(&shared));
        spawn_name_watch_thread(Arc::clone(&shared), tx.clone());

        // Reconcile banners against queue state on every change. This
        // is what closes a banner when the request behind it was
        // resolved by something that isn't us — `vet daemon approve`
        // over the admin socket, an `AddRule` that auto-approved it,
        // or shutdown's `cancel_all`.
        //
        // Registered through `add_change_listener`, not
        // `set_change_listener`: Phase 6c's tray item needs the same
        // notifications and 6d's window will be a third consumer, so
        // the queue appends rather than replacing. (Resolved in 6c —
        // this used to be a note warning whoever landed 6d that they
        // would silently disable banner-closing by overwriting the
        // single slot that existed then.)
        let listener_tx = tx.clone();
        shared.queue.add_change_listener(move || {
            // Fire-and-forget: a full channel or a torn-down worker
            // must never propagate back into `PendingQueue::resolve`.
            let _ = listener_tx.send(Job::Reconcile);
        });

        Ok(Self {
            shared,
            jobs: Mutex::new(tx),
        })
    }
}

/// Outgoing-call thread. Owns every `Notify` / `CloseNotification` so
/// callers never block on the bus.
fn spawn_jobs_thread(shared: Arc<Shared>, rx: std::sync::mpsc::Receiver<Job>) {
    std::thread::Builder::new()
        .name("vetterd-notify".into())
        .spawn(move || {
            for job in rx {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                match job {
                    Job::Post(summary) => shared.post(&summary),
                    Job::Reconcile => shared.reconcile(),
                    Job::RefreshCapabilities => shared.refresh_capabilities(),
                    Job::Stop => return,
                }
            }
        })
        .expect("spawn notification job thread");
}

/// Inbound-signal thread for the Notifications interface.
fn spawn_signal_thread(shared: Arc<Shared>) {
    std::thread::Builder::new()
        .name("vetterd-notify-sig".into())
        .spawn(move || {
            let proxy = match shared.proxy() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("vetterd: notifications: signal proxy: {e}");
                    return;
                }
            };
            let signals = match proxy.receive_all_signals() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("vetterd: notifications: subscribe failed: {e}");
                    return;
                }
            };
            for msg in signals {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                let Some(member) = msg.header().member().map(|m| m.to_string()) else {
                    continue;
                };
                match member.as_str() {
                    "ActionInvoked" => {
                        if let Ok((notification, key)) = msg.body().deserialize::<(u32, String)>() {
                            on_action_invoked(&shared, notification, &key);
                        }
                    }
                    "NotificationClosed" => {
                        // (id, reason). The server has already
                        // removed the banner; just drop the pairing
                        // so we never try to close it again. The
                        // request itself stays pending — dismissing a
                        // banner is not a decision.
                        if let Ok((notification, _reason)) = msg.body().deserialize::<(u32, u32)>()
                        {
                            shared
                                .map
                                .lock()
                                .expect("notification map poisoned")
                                .forget_notification(notification);
                            // Any token parked for this banner can
                            // never be claimed now: the interaction
                            // it authorised is over.
                            shared
                                .tokens
                                .lock()
                                .expect("token map poisoned")
                                .remove(&notification);
                        }
                    }
                    "ActivationToken" => {
                        // (id, token). Plasma emits this immediately
                        // before the `ActionInvoked` it belongs to,
                        // so park it for that handler to collect
                        // (§5.6). Without it a window raised from a
                        // banner click trips Wayland's focus-stealing
                        // prevention and opens behind whatever the
                        // user was looking at.
                        if let Ok((notification, token)) = msg.body().deserialize::<(u32, String)>()
                        {
                            let mut tokens = shared.tokens.lock().expect("token map poisoned");
                            prune_tokens(&mut tokens);
                            tokens.insert(notification, token);
                        }
                    }
                    _ => {}
                }
            }
        })
        .expect("spawn notification signal thread");
}

/// Watches the notification server joining / leaving the bus.
fn spawn_name_watch_thread(shared: Arc<Shared>, jobs: Sender<Job>) {
    std::thread::Builder::new()
        .name("vetterd-notify-name".into())
        .spawn(move || {
            let dbus = match DBusProxy::new(&shared.conn) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("vetterd: notifications: DBus proxy: {e}");
                    return;
                }
            };
            let changes = match dbus.receive_name_owner_changed() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("vetterd: notifications: NameOwnerChanged subscribe: {e}");
                    return;
                }
            };
            for change in changes {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(args) = change.args() else { continue };
                if args.name().as_str() != NOTIFY_NAME {
                    continue;
                }
                // The server restarted (or first appeared). Its
                // notification ids mean nothing to the new owner, so
                // drop the map before re-querying — §11's "rebuild it
                // if the daemon disappears from the bus".
                let dropped = {
                    let mut map = shared.map.lock().expect("notification map poisoned");
                    let n = map.len();
                    map.clear();
                    n
                };
                if dropped > 0 {
                    eprintln!(
                        "vetterd: notifications: server changed owner; dropped \
                         {dropped} stale notification id(s)"
                    );
                }
                let _ = jobs.send(Job::RefreshCapabilities);
            }
        })
        .expect("spawn notification name-watch thread");
}

/// Resolve the request behind `notification` per the action the user
/// clicked. Runs on the signal thread; `PendingQueue::resolve` is
/// thread-safe, so no hop is needed (§11).
fn on_action_invoked(shared: &Arc<Shared>, notification: u32, key: &str) {
    // Claim any token the server parked for this interaction, whether
    // or not we end up needing it: leaving it behind would keep a
    // stale token for a banner that is about to be resolved and
    // closed.
    let token = shared
        .tokens
        .lock()
        .expect("token map poisoned")
        .remove(&notification);

    let request = shared
        .map
        .lock()
        .expect("notification map poisoned")
        .request_for(notification)
        .map(str::to_string);

    match click_outcome(key, request.as_deref()) {
        ClickOutcome::Nothing => {}
        ClickOutcome::Resolve { request, decision } => {
            // The change listener fires from inside `resolve` and
            // queues the `CloseNotification`, so there is nothing to
            // close here.
            shared.queue.resolve(&request, decision);
        }
        ClickOutcome::Show { card } => {
            // Deliberately *not* a resolve: the request stays parked
            // and its banner stays up. See [`ActionIntent`].
            crate::runloop::request_show_for(card, token);
        }
    }
}

/// Keep the parked-token map from growing without bound.
///
/// A token is normally claimed microseconds later by the
/// `ActionInvoked` it precedes, so the map holds at most one entry in
/// steady state. The cases that leak are servers that emit a token
/// for an interaction which never becomes an action. Rather than
/// track wall-clock ages for something this small, drop everything
/// once the map is implausibly large: any token still parked by then
/// is long past the moment the compositor would have honoured it.
fn prune_tokens(tokens: &mut HashMap<u32, String>) {
    const MAX_PARKED_TOKENS: usize = 32;
    if tokens.len() >= MAX_PARKED_TOKENS {
        tokens.clear();
    }
}

impl Notifier for LinuxNotifier {
    fn notify(&self, summary: &PromptSummary, hint: NotifyHint) {
        if !should_raise_banner(hint) {
            return;
        }
        let job = Job::Post(Box::new(summary.clone()));
        if self
            .jobs
            .lock()
            .expect("jobs sender poisoned")
            .send(job)
            .is_err()
        {
            eprintln!(
                "vetterd: notifications: job thread is gone; no banner for id `{}`",
                summary.id
            );
        }
    }

    fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Best-effort: close anything still on screen before the
        // daemon exits, then stop the worker. `cancel_all` has
        // already emptied the queue by the time this runs, so
        // `reconcile` closes every banner we still hold.
        let jobs = self.jobs.lock().expect("jobs sender poisoned");
        let _ = jobs.send(Job::Reconcile);
        let _ = jobs.send(Job::Stop);
        // NOTE: deliberately *not* `clear_change_listener()` — that
        // clears every slot, including the tray's (Phase 6c). Our own
        // listener is already harmless once the jobs thread stops:
        // it only does `tx.send(Job::Reconcile)`, whose failure is
        // ignored, so a late resolve costs one dropped send rather
        // than reaching into a torn-down worker.
    }
}

#[cfg(test)]
#[path = "../tests/notifier_linux.rs"]
mod tests;
