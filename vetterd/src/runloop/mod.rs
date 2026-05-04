//! macOS AppKit run loop driver.
//!
//! Owned by the daemon's main thread when the binary runs as the
//! `Vetter.app` bundle (i.e. with `VETTERD_NOTIFIER=mac`). Sets up
//! `NSApplication`, a `LSUIElement`-style accessory activation
//! policy, a menu-bar `NSStatusItem` with a [`status_item`] icon,
//! the [`popover`] approval surface, and a delegate object that
//! simultaneously satisfies `NSApplicationDelegate` and
//! `UNUserNotificationCenterDelegate`. The delegate is what carries
//! the [`PendingQueue`] handle into Cocoa-land and resolves it from
//! the `didReceiveNotificationResponse:` callback (and from the
//! popover's per-card buttons).
//!
//! Why one delegate for both protocols: `NSApplication` retains its
//! delegate strongly, but `UNUserNotificationCenter::setDelegate` is
//! a *weak* property. Pinning the same Retained on the application
//! delegate slot keeps the notification delegate alive without us
//! needing to leak a static `Retained<...>` of our own.
//!
//! Shutdown: SIGTERM/SIGINT flips the daemon's shutdown flag and the
//! signal-handler thread also dispatches `NSApplication::terminate`
//! to the main queue so `NSApplication::run()` returns.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use block2::{DynBlock, RcBlock};
use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::DefinedClass;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSEvent,
    NSEventModifierFlags, NSEventType,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSError, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSSet,
    NSString,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationDefaultActionIdentifier, UNNotificationDismissActionIdentifier,
    UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::pending::{PendingDecision, PendingQueue};
use crate::Context;

mod popover;
pub(crate) mod popover_attr;
pub(crate) mod popover_effects;
pub(crate) mod popover_picker;
pub(crate) mod popover_pills;
pub(crate) mod popover_url;
mod status_item;

use popover::Popover;
use status_item::StatusItem;

/// Stable identifiers for the action buttons. Kept in lockstep with
/// the Info.plist (the bundle declares the same category id for the
/// notifications it ships).
///
/// Two categories: the base id carries Approve/Reject/Allowlist…
/// (the popover gates `Trust host…` on the request actually having
/// an `UnknownHost` signal, so the banner mirrors that gating by
/// stamping the with-unknown-host id only when the signal is
/// present). The category is picked per-request inside
/// [`crate::notifier::mac::MacNotifier::notify`] and threaded into
/// [`post_notification`] as `has_unknown_host`.
pub const CATEGORY_ID: &str = "vetter.prompt";
pub const CATEGORY_ID_WITH_UNKNOWN_HOST: &str = "vetter.prompt.with_unknown_host";
pub const ACTION_APPROVE: &str = "vetter.approve";
pub const ACTION_REJECT: &str = "vetter.reject";
pub const ACTION_ALLOWLIST: &str = "vetter.allowlist";
pub const ACTION_TRUST_HOST: &str = "vetter.trust_host";

/// Reason recorded on the audit log when the user resolves via the
/// popover instead of the notification banner.
const REASON_POPOVER_APPROVE: &str = "approved via popover";
const REASON_POPOVER_REJECT: &str = "rejected via popover";

/// Main-thread entry point for the AppKit-driven notifier.
///
/// Blocks until `NSApplication::run()` returns. We trigger the
/// return via [`stop_run_loop`] (`[NSApp stop:]` plus a dummy event
/// to wake `nextEventMatchingMask:`) — **never** via `terminate:`.
/// `terminate:` calls `exit()` after the standard delegate
/// ceremony and would skip the cleanup code in
/// [`crate::run`] that removes the socket and pidfile.
///
/// The caller is expected to have already spawned the daemon's
/// accept-loop on a background thread.
pub fn run_app_kit(ctx: Arc<Context>, shutdown: Arc<AtomicBool>) {
    let mtm = MainThreadMarker::new()
        .expect("runloop::run_app_kit must be called on the process main thread");

    let app = NSApplication::sharedApplication(mtm);
    // Accessory: dock-less menu-bar app. `LSUIElement=true` in the
    // bundle's Info.plist gives the same effect when launched via
    // `open Vetter.app`; setting it programmatically here covers the
    // `cargo run --release --bin vetterd` dev path too.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let queue = Arc::clone(&ctx.pending);
    let delegate = AppDelegate::new(mtm, Arc::clone(&ctx), Arc::clone(&shutdown));
    let proto = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(proto));

    // Watch the shutdown atomic and translate it into a
    // `[NSApp stop:]` once it flips. Both SIGTERM/SIGINT (via
    // signal-hook) and the popover's Quit button feed the atomic;
    // funnelling everything through one observer means the cleanup
    // path is identical regardless of how shutdown was triggered.
    //
    // 250ms cadence is short enough that human-driven Quit feels
    // instant and long enough to keep idle CPU near zero.
    let shutdown_for_main = Arc::clone(&shutdown);
    std::thread::spawn(move || loop {
        if shutdown_for_main.load(Ordering::SeqCst) {
            DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new()
                    .expect("shutdown observer dispatched onto the main queue");
                stop_run_loop(mtm);
            });
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    });

    // app.run() returns once `stop_run_loop` fires (above). Then we
    // fall through to the cleanup tail in `vetterd::run`.
    app.run();

    // Wake any workers still parked on the queue with a deny.
    queue.cancel_all();
}

/// Stop the AppKit run loop without invoking `[NSApp terminate:]`.
///
/// `[NSApp stop:]` flips an internal flag that
/// `nextEventMatchingMask:` checks before returning the next event.
/// If the queue is empty (the menu-bar app sits idle most of the
/// time), the loop will block in Mach kernel space and never
/// observe the flag — so we also post a no-op application-defined
/// event to guarantee the loop wakes promptly. The combination is
/// the standard AppKit recipe for "graceful return from
/// `[NSApp run]`".
fn stop_run_loop(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags(0),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(event) = event {
        app.postEvent_atStart(&event, true);
    }
}

/// Post a single notification on the main queue. Worker threads call
/// this through [`crate::notifier::mac::MacNotifier::notify`].
///
/// `has_unknown_host` selects between the two registered categories:
/// the with-unknown-host id includes the `Trust host…` action, the
/// base id stops at `Allowlist…`. Mirrors the popover's per-card
/// gating so the banner doesn't offer Trust host… for already-known
/// hosts.
pub(crate) fn post_notification(
    id: String,
    command: String,
    primary_verb: String,
    primary_target: String,
    force_prompt: bool,
    has_unknown_host: bool,
) {
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().expect("dispatched onto main");
        let _ = mtm; // silence unused warning if no MTM-only API ends up needed below.
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let content = UNMutableNotificationContent::new();
        let title = format!("vet {command}");
        let body = if primary_verb.is_empty() {
            primary_target.clone()
        } else {
            format!("{primary_verb} {primary_target}")
        };
        let category_id = if has_unknown_host {
            CATEGORY_ID_WITH_UNKNOWN_HOST
        } else {
            CATEGORY_ID
        };
        content.setTitle(&NSString::from_str(&title));
        content.setBody(&NSString::from_str(&body));
        content.setCategoryIdentifier(&NSString::from_str(category_id));
        if force_prompt {
            content.setSubtitle(&NSString::from_str("dry run"));
        }

        let req_id = NSString::from_str(&id);
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&req_id, &content, None);
        let id_for_log = id.clone();
        let handler: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(move |err: *mut NSError| {
            if !err.is_null() {
                eprintln!("vetterd: addNotificationRequest failed for id `{id_for_log}` (raw err)");
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&*handler));
    });
}

/// Remove any delivered banners for `ids` so resolving via the
/// popover (or by clicking through the banner body) doesn't leave a
/// stale notification card sitting in Notification Center. Called
/// from both the notification-action callback and the popover's
/// per-card buttons.
pub(crate) fn remove_delivered_for_ids(ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    DispatchQueue::main().exec_async(move || {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let nss: Vec<Retained<NSString>> = ids.iter().map(|s| NSString::from_str(s)).collect();
        let arr = NSArray::from_retained_slice(&nss);
        center.removeDeliveredNotificationsWithIdentifiers(&arr);
    });
}

/// One-shot main-thread setup hook used by the AppDelegate's
/// `applicationDidFinishLaunching:` handler.
fn install_notification_machinery(_mtm: MainThreadMarker, delegate: &AppDelegate) {
    let center = UNUserNotificationCenter::currentNotificationCenter();

    // Register ourselves as the delegate. The center holds the
    // delegate weakly; the NSApplication strong reference keeps it
    // alive for the lifetime of the process.
    let proto = ProtocolObject::from_ref(delegate);
    center.setDelegate(Some(proto));

    // Action buttons: Approve (foreground), Reject (destructive),
    // Allowlist… (foreground — opens the picker NSAlert), and the
    // optional Trust host… (foreground, only attached to the with-
    // unknown-host category). Approve/Reject lead so they stay in
    // the inline two-to-three-button banner layout; the picker
    // actions condense under macOS's "Options" dropdown when there
    // are too many to fit inline.
    let approve = UNNotificationAction::actionWithIdentifier_title_options(
        &NSString::from_str(ACTION_APPROVE),
        &NSString::from_str("Approve"),
        UNNotificationActionOptions::Foreground,
    );
    let reject = UNNotificationAction::actionWithIdentifier_title_options(
        &NSString::from_str(ACTION_REJECT),
        &NSString::from_str("Reject"),
        UNNotificationActionOptions::Destructive,
    );
    let allowlist = UNNotificationAction::actionWithIdentifier_title_options(
        &NSString::from_str(ACTION_ALLOWLIST),
        &NSString::from_str("Allowlist…"),
        UNNotificationActionOptions::Foreground,
    );
    let trust_host = UNNotificationAction::actionWithIdentifier_title_options(
        &NSString::from_str(ACTION_TRUST_HOST),
        &NSString::from_str("Trust host…"),
        UNNotificationActionOptions::Foreground,
    );

    let intents: Retained<NSArray<NSString>> = NSArray::new();
    let base_actions =
        NSArray::from_retained_slice(&[approve.clone(), reject.clone(), allowlist.clone()]);
    let base_category =
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(CATEGORY_ID),
            &base_actions,
            &intents,
            UNNotificationCategoryOptions::CustomDismissAction,
        );
    let with_host_actions = NSArray::from_retained_slice(&[approve, reject, allowlist, trust_host]);
    let with_host_category =
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(CATEGORY_ID_WITH_UNKNOWN_HOST),
            &with_host_actions,
            &intents,
            UNNotificationCategoryOptions::CustomDismissAction,
        );
    let categories = NSSet::from_retained_slice(&[base_category, with_host_category]);
    center.setNotificationCategories(&categories);

    // Authorization request. First-run pops the system permission
    // dialog; thereafter it returns the previously-granted answer.
    // We log a warning if not granted but don't block startup —
    // the notifier still does its best.
    let auth_block: RcBlock<dyn Fn(objc2::runtime::Bool, *mut NSError)> =
        RcBlock::new(move |granted: objc2::runtime::Bool, _err: *mut NSError| {
            if !granted.as_bool() {
                eprintln!(
                    "vetterd: notification authorization not granted; \
                     prompts may not be visible"
                );
            }
        });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &auth_block,
    );
}

/// Ivars on the `AppDelegate` Objective-C class. Wrapped in interior
/// mutability so the delegate methods can mutate without `&mut self`.
#[derive(Default)]
pub struct AppDelegateIvars {
    /// Full daemon context. Used by the popover's Phase-5 picker
    /// flow to call [`crate::suggestions::add_allowlist_rule`] and
    /// [`crate::suggestions::add_known_host`] on a background
    /// dispatch (file IO + reload happens off the main thread).
    /// The pending queue is reachable through `ctx.pending` so we
    /// only carry one shared handle here.
    ctx: std::sync::OnceLock<Arc<Context>>,
    /// Shared shutdown flag. SIGTERM/SIGINT (via signal-hook) and
    /// the popover's Quit button both flip this to `true`; the
    /// observer thread spawned in `run_app_kit` notices the flip
    /// and calls [`stop_run_loop`] to make `app.run()` return.
    shutdown: std::sync::OnceLock<Arc<AtomicBool>>,
    status_item: std::sync::OnceLock<StatusItem>,
    popover: std::sync::OnceLock<Popover>,
    /// The pending request id the most recent notification body
    /// click-through was for. Read by the popover the next time it
    /// shows so it can scroll the matching card into view; cleared
    /// after each show. `Mutex<Option<String>>` is overkill (the
    /// access pattern is single-threaded on the main queue) but it
    /// lets the field stay private to this module without needing a
    /// `Cell`-style escape hatch on the `OnceLock` storage.
    focused_id: Mutex<Option<String>>,
}

impl AppDelegateIvars {
    pub(crate) fn ctx(&self) -> &Arc<Context> {
        self.ctx
            .get()
            .expect("ctx is set immediately after AppDelegate::new")
    }

    pub(crate) fn queue(&self) -> &Arc<PendingQueue> {
        &self.ctx().pending
    }

    pub(crate) fn take_focused_id(&self) -> Option<String> {
        self.focused_id
            .lock()
            .expect("focused_id mutex poisoned")
            .take()
    }

    fn set_focused_id(&self, id: String) {
        *self.focused_id.lock().expect("focused_id mutex poisoned") = Some(id);
    }
}

define_class!(
    // SAFETY: superclass NSObject has no subclass requirements;
    // AppDelegate does not implement Drop and stores only Send/Sync
    // ivars (Arc, OnceLock, Mutex).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VetterAppDelegate"]
    #[ivars = AppDelegateIvars]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: `NSApplicationDelegate` has no extra safety requirements.
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            install_notification_machinery(mtm, self);

            // Build the popover and the menu-bar status item.
            // The popover holds a strong ref to a controller object;
            // the status item button's action targets `self` and
            // calls into `togglePopover:`.
            let popover = Popover::new(mtm, Arc::clone(self.ivars().ctx()), self);
            self.ivars().popover.set(popover).ok();

            let status = StatusItem::install(mtm, self);
            self.ivars().status_item.set(status).ok();

            // Wire up the change-listener so submits/resolves/cancels
            // refresh both the status-item badge and the popover (if
            // it's open). The listener runs on whatever thread fired
            // the change; we hop to the main queue to touch AppKit.
            //
            // The listener must be `Send + Sync + 'static`, but a
            // `Retained<AppDelegate>` is `!Send` (objc2 won't
            // blanket-promise that arbitrary Obj-C objects are
            // thread-safe). `MainThreadBound` is the typed escape
            // hatch: it's unconditionally `Send + Sync`, but the
            // only way to read the inner value is through
            // `get(&mtm)`, which the borrow checker won't let us
            // call off the main thread. So the "must touch AppKit
            // from main only" rule is enforced statically rather
            // than by a comment + raw pointer.
            //
            // We retain `self` once at registration so the
            // captured handle is independent of NSApp's delegate
            // slot — if the delegate is ever swapped out (it
            // isn't today, but defensive) the listener still has
            // a valid handle until it's dropped.
            let strong: Retained<AppDelegate> = unsafe {
                // SAFETY: `self` is a live `&AppDelegate` reached
                // from `applicationDidFinishLaunching:`; retaining
                // it bumps the refcount and gives us an owned
                // handle the listener closure can carry.
                Retained::retain(self as *const _ as *mut AppDelegate)
                    .expect("self is non-null and live in didFinishLaunching")
            };
            let bound = Arc::new(MainThreadBound::new(strong, mtm));
            self.ivars().queue().set_change_listener(move || {
                let bound = Arc::clone(&bound);
                DispatchQueue::main().exec_async(move || {
                    let mtm = MainThreadMarker::new()
                        .expect("change-listener dispatched onto the main queue");
                    bound.get(mtm).refresh_ui();
                });
            });

            // Initial paint so the badge shows zero pending instead
            // of an empty string.
            self.refresh_ui();
        }
    }

    // SAFETY: `UNUserNotificationCenterDelegate` has no extra safety
    // requirements; the methods below have the canonical signatures
    // declared by the protocol.
    unsafe impl UNUserNotificationCenterDelegate for AppDelegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let action = response.actionIdentifier();
            let id = response.notification().request().identifier();
            let id = id.to_string();

            let action = action.to_string();
            let default = unsafe { UNNotificationDefaultActionIdentifier.to_string() };
            let dismiss = unsafe { UNNotificationDismissActionIdentifier.to_string() };

            if action == ACTION_APPROVE {
                self.ivars()
                    .queue()
                    .resolve(&id, PendingDecision::allow("approved via notification"));
                remove_delivered_for_ids(vec![id.clone()]);
            } else if action == ACTION_REJECT {
                self.ivars()
                    .queue()
                    .resolve(&id, PendingDecision::deny("rejected via notification"));
                remove_delivered_for_ids(vec![id.clone()]);
            } else if action == ACTION_ALLOWLIST {
                // Banner-side `Allowlist…`: lift the same picker
                // sheet the popover Allowlist… button uses. Leaves
                // the queue entry pending — `persist_rule_async`
                // auto-approves it (and clears the banner) only if
                // the user picks a rule that covers it.
                self.handle_allowlist_action(id);
            } else if action == ACTION_TRUST_HOST {
                // Banner-side `Trust host…`: lift the host picker.
                // Trusting a host never auto-approves the pending
                // request (the popover's `add_known_host` path has
                // the same property), so the banner stays until
                // the user picks Approve/Reject themselves.
                self.handle_trust_host_action(id);
            } else if action == dismiss {
                self.ivars().queue().resolve(
                    &id,
                    PendingDecision::deny("dismissed via notification (treated as reject)"),
                );
                remove_delivered_for_ids(vec![id.clone()]);
            } else if action == default {
                // User clicked the body of the banner. Open the
                // popover so they can review and Approve/Reject from
                // a real UI surface; leave the queue entry pending.
                self.ivars().set_focused_id(id.clone());
                self.show_popover_anchored();
            } else {
                eprintln!("vetterd: unknown action `{action}` for id `{id}`");
            }

            completion_handler.call(());
        }
    }

    impl AppDelegate {
        /// Action wired to the menu-bar status-item button.
        /// Toggles the popover's open/closed state.
        #[unsafe(method(togglePopover:))]
        fn toggle_popover_action(&self, _sender: Option<&NSObject>) {
            self.toggle_popover();
        }

        /// Action wired to the popover's "Quit Vetter" button.
        ///
        /// Flips the shared shutdown atomic so the observer thread
        /// in `run_app_kit` calls `stop_run_loop`, returning control
        /// to `app.run()` and letting `vetterd::run`'s cleanup tail
        /// remove the socket and pidfile. This is the **only**
        /// shutdown entry point from the UI: do not target
        /// `[NSApp terminate:]` directly, which would call `exit()`
        /// and leak runtime files.
        #[unsafe(method(requestShutdown:))]
        fn request_shutdown_action(&self, _sender: Option<&NSObject>) {
            if let Some(flag) = self.ivars().shutdown.get() {
                flag.store(true, Ordering::SeqCst);
            }
        }

        /// Action wired to the popover's "Start at login" checkbox.
        ///
        /// Reads the new state from `[sender state]`, persists it
        /// to `~/.vet/settings.yaml`, then calls
        /// `SMAppService.{register,unregister}` to converge the OS.
        /// On either failure we re-sync the checkbox with the
        /// live OS state so the UI never claims a setting the OS
        /// rejected (e.g. the user has not yet approved Vetter in
        /// System Settings → Login Items).
        ///
        /// Runs entirely on the main thread — `objc2_app_kit`
        /// guarantees the action is delivered there, and
        /// SMAppService calls are documented as safe from any
        /// queue. The settings write is a small synchronous YAML
        /// rewrite; we don't hop to a background queue because
        /// the user has explicit feedback (the checkbox visibly
        /// snaps back on failure) and stalling the main thread
        /// for a ~5ms `~/.vet/settings.yaml` write is preferable
        /// to having the UI lie about the persisted state in the
        /// race window.
        #[unsafe(method(toggleAutostart:))]
        fn toggle_autostart_action(&self, sender: Option<&objc2_app_kit::NSButton>) {
            let Some(button) = sender else {
                eprintln!("vetterd: toggleAutostart: invoked with nil sender");
                return;
            };
            let want_on =
                button.state() == objc2_app_kit::NSControlStateValueOn;
            self.apply_autostart_change(want_on);
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, ctx: Arc<Context>, shutdown: Arc<AtomicBool>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        let this: Retained<Self> = unsafe { objc2::msg_send![super(this), init] };
        this.ivars().ctx.set(ctx).ok();
        this.ivars().shutdown.set(shutdown).ok();
        this
    }

    /// Recompute the menu-bar badge and (if the popover is open)
    /// rebuild its card list from the queue's current state. Cheap:
    /// the queue snapshot is a `Vec<(PromptSummary, String)>`. Does
    /// not consume the focused id; that's reserved for the
    /// click-through path which calls `show_popover_anchored`.
    fn refresh_ui(&self) {
        let (pending, resolved) = self.ivars().queue().all_entries();
        if let Some(status) = self.ivars().status_item.get() {
            status.set_pending_count(pending.len());
        }
        if let Some(popover) = self.ivars().popover.get() {
            popover.refresh(&pending, &resolved, None);
        }
    }

    /// Persist `enabled` to `~/.vet/settings.yaml` and converge the
    /// OS-level Login Item state via [`crate::autostart`]. On any
    /// failure we re-sync the popover checkbox with the live OS
    /// state so the UI never claims a setting the OS rejected.
    ///
    /// We don't surface a modal dialog on success — the click *is*
    /// the feedback (checkbox state stays flipped). On failure we
    /// log to stderr and rely on the rollback to make the failure
    /// visible. A future iteration may want a "could not register
    /// at login" `NSAlert`, but that's out of scope for the
    /// initial cut.
    fn apply_autostart_change(&self, enabled: bool) {
        let mut settings = vetter_core::settings::load().unwrap_or_default();
        if settings.autostart != enabled {
            settings.autostart = enabled;
            if let Err(e) = vetter_core::settings::store(&settings) {
                eprintln!("vetterd: persist autostart preference failed: {e}");
                if let Some(popover) = self.ivars().popover.get() {
                    popover.refresh_autostart_checkbox();
                }
                return;
            }
        }
        let result = if enabled {
            crate::autostart::enable()
        } else {
            crate::autostart::disable()
        };
        if let Err(e) = result {
            eprintln!("vetterd: SMAppService toggle failed: {e}");
            // Resync the visible state with what the OS actually
            // shows — the user should never see the checkbox claim
            // a state the OS contradicts.
            if let Some(popover) = self.ivars().popover.get() {
                popover.refresh_autostart_checkbox();
            }
            return;
        }
        // Success path: the state we just wrote should match the
        // OS. Refresh anyway so RequiresApproval (which is a
        // documented post-`register` state on first install) is
        // reflected — `is_enabled()` returns true for both
        // `Enabled` and `RequiresApproval`, so the checkbox
        // stays on without lying.
        if let Some(popover) = self.ivars().popover.get() {
            popover.set_autostart_checkbox_state(enabled);
        }
    }

    /// Open the popover anchored to the status-item button. Honours
    /// any focused id set by a notification click-through so the
    /// matching card is scrolled into view; the focused id is
    /// consumed (cleared) on read.
    fn show_popover_anchored(&self) {
        let focused = self.ivars().take_focused_id();
        if let (Some(popover), Some(status)) =
            (self.ivars().popover.get(), self.ivars().status_item.get())
        {
            let (pending, resolved) = self.ivars().queue().all_entries();
            popover.refresh(&pending, &resolved, focused.as_deref());
            popover.show_relative_to(status.button());
        }
    }

    /// Toggle the popover anchored to the status-item button.
    /// Toggle never honours a focused id — the user opened it
    /// themselves, so don't auto-scroll past their cursor.
    fn toggle_popover(&self) {
        if let (Some(popover), Some(status)) =
            (self.ivars().popover.get(), self.ivars().status_item.get())
        {
            if popover.is_shown() {
                popover.close();
            } else {
                let (pending, resolved) = self.ivars().queue().all_entries();
                popover.refresh(&pending, &resolved, None);
                popover.show_relative_to(status.button());
            }
        }
    }

    /// Body of the banner-side `Allowlist…` action. Looks up the
    /// pending request, asks the suggestion engine for its rule
    /// candidates, activates the app so the modal alert lands in
    /// front, and lifts the same picker the popover Allowlist…
    /// button uses. Empty / unknown id silently no-ops — the
    /// banner button might fire after the request was already
    /// auto-approved by another path.
    fn handle_allowlist_action(&self, id: String) {
        let ctx = Arc::clone(self.ivars().ctx());
        let Some((allowlist, _)) = crate::suggestions::suggestions_for(&ctx, &id) else {
            return;
        };
        if allowlist.is_empty() {
            return;
        }
        let mtm = self.mtm();
        activate_app(mtm);
        popover_picker::show_allowlist_picker(mtm, ctx, allowlist);
    }

    /// Body of the banner-side `Trust host…` action. Mirror of
    /// `handle_allowlist_action` for the host picker. Same silent
    /// no-op rule on unknown id / empty suggestions.
    fn handle_trust_host_action(&self, id: String) {
        let ctx = Arc::clone(self.ivars().ctx());
        let Some((_, host)) = crate::suggestions::suggestions_for(&ctx, &id) else {
            return;
        };
        if host.is_empty() {
            return;
        }
        let mtm = self.mtm();
        activate_app(mtm);
        popover_picker::show_host_picker(mtm, ctx, host);
    }
}

/// Pull the application to the foreground before showing a modal
/// `NSAlert`. `LSUIElement=true` accessory apps do not reliably
/// auto-foreground when a `UNNotificationAction` fires, even with
/// the action's `Foreground` option set — without an explicit
/// `activateIgnoringOtherApps:`, `runModal()` yields a window that
/// sits *behind* the frontmost app and is easy to miss.
///
/// We use the pre-macOS-14 selector so the call works on the
/// bundle's declared `LSMinimumSystemVersion=11.0` floor; the
/// `#[allow(deprecated)]` is intentional. The replacement
/// `-[NSApplication activate]` was added in macOS 14 and would
/// silently no-op on older systems.
fn activate_app(mtm: MainThreadMarker) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

/// Public re-export for the popover module: the popover needs to
/// resolve queue entries by id when its buttons are clicked.
pub(crate) fn resolve(queue: &PendingQueue, id: &str, allow: bool) {
    let decision = if allow {
        PendingDecision::allow(REASON_POPOVER_APPROVE)
    } else {
        PendingDecision::deny(REASON_POPOVER_REJECT)
    };
    queue.resolve(id, decision);
    remove_delivered_for_ids(vec![id.to_string()]);
}
