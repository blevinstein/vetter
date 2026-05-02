//! macOS AppKit run loop driver.
//!
//! Owned by the daemon's main thread when the binary runs as the
//! `Vetter.app` bundle (i.e. with `VETTERD_NOTIFIER=mac`). Sets up
//! `NSApplication`, a `LSUIElement`-style accessory activation
//! policy, a minimal menu-bar `NSStatusItem`, and a delegate object
//! that simultaneously satisfies `NSApplicationDelegate` and
//! `UNUserNotificationCenterDelegate`. The delegate is what carries
//! the [`PendingQueue`] handle into Cocoa-land and resolves it from
//! the `didReceiveNotificationResponse:` callback.
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
use std::sync::Arc;

use block2::{DynBlock, RcBlock};
use dispatch2::DispatchQueue;
use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::sel;
use objc2::DefinedClass;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuItem,
    NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSError, NSNotification, NSObject, NSObjectProtocol,
    NSSet, NSString,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationDefaultActionIdentifier, UNNotificationDismissActionIdentifier,
    UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::pending::{PendingDecision, PendingQueue};

/// Stable identifiers for the action buttons. Kept in lockstep with
/// the Info.plist (the bundle declares the same category id for the
/// notifications it ships).
pub const CATEGORY_ID: &str = "vetter.prompt";
pub const ACTION_APPROVE: &str = "vetter.approve";
pub const ACTION_REJECT: &str = "vetter.reject";

/// Main-thread entry point for the AppKit-driven notifier.
///
/// Blocks until `NSApplication::run()` returns (i.e. after a
/// `terminate:` from the signal handler or the menu-bar Quit item).
/// The caller is expected to have already spawned the daemon's
/// accept-loop on a background thread.
pub fn run_app_kit(queue: Arc<PendingQueue>, shutdown: Arc<AtomicBool>) {
    let mtm = MainThreadMarker::new()
        .expect("runloop::run_app_kit must be called on the process main thread");

    let app = NSApplication::sharedApplication(mtm);
    // Accessory: dock-less menu-bar app. `LSUIElement=true` in the
    // bundle's Info.plist gives the same effect when launched via
    // `open Vetter.app`; setting it programmatically here covers the
    // `cargo run --release --bin vetterd` dev path too.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = AppDelegate::new(mtm, Arc::clone(&queue));
    let proto = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(proto));

    // Listen for the shutdown atomic flipping and translate it into
    // an NSApp.terminate. We poll on a low-frequency timer rather
    // than wiring a dispatch source so the scheduling stays
    // dependency-free (signal-hook already owns SIGTERM/SIGINT).
    let app_for_shutdown = app.clone();
    let shutdown_for_main = Arc::clone(&shutdown);
    std::thread::spawn(move || loop {
        if shutdown_for_main.load(Ordering::SeqCst) {
            DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new().expect("dispatched onto main");
                let app = NSApplication::sharedApplication(mtm);
                app.terminate(None);
                let _ = app_for_shutdown; // keep retained on this thread until use
            });
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    });

    // app.run() never returns under normal operation; on
    // `terminate:` it returns and we fall through.
    app.run();

    // Wake any workers still parked on the queue with a deny.
    queue.cancel_all();
}

/// Post a single notification on the main queue. Worker threads call
/// this through [`crate::notifier::mac::MacNotifier::notify`].
pub(crate) fn post_notification(
    id: String,
    command: String,
    primary_verb: String,
    primary_target: String,
    force_prompt: bool,
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
        content.setTitle(&NSString::from_str(&title));
        content.setBody(&NSString::from_str(&body));
        content.setCategoryIdentifier(&NSString::from_str(CATEGORY_ID));
        if force_prompt {
            content.setSubtitle(&NSString::from_str("dry run"));
        }

        let req_id = NSString::from_str(&id);
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&req_id, &content, None);
        // Block prints any post error to stderr but otherwise lets
        // the worker time out / fall back through `cancel_all` on
        // shutdown.
        let id_for_log = id.clone();
        let handler: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(move |err: *mut NSError| {
            if !err.is_null() {
                eprintln!("vetterd: addNotificationRequest failed for id `{id_for_log}` (raw err)");
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&*handler));
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

    // Action buttons: Approve (foreground) and Reject (destructive).
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

    let actions = NSArray::from_retained_slice(&[approve, reject]);
    let intents: Retained<NSArray<NSString>> = NSArray::new();
    let category = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(CATEGORY_ID),
        &actions,
        &intents,
        UNNotificationCategoryOptions::CustomDismissAction,
    );
    let categories = NSSet::from_retained_slice(&[category]);
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

/// One-shot main-thread setup hook for the menu-bar status item.
fn install_status_item(mtm: MainThreadMarker, ivars: &AppDelegateIvars) {
    let bar = NSStatusBar::systemStatusBar();
    let item = bar.statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        button.setTitle(ns_string!("Vetter"));
    }

    // Single "Quit Vetter" menu item. The popover with pending
    // requests is deferred to a follow-up Phase 4 PR.
    let menu = NSMenu::new(mtm);
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Quit Vetter"),
            Some(sel!(terminate:)),
            ns_string!("q"),
        )
    };
    menu.addItem(&quit);
    item.setMenu(Some(&menu));

    // Stash the status item on the delegate so the system retains it
    // for the lifetime of the process; an unanchored `NSStatusItem`
    // is silently released and disappears.
    ivars.status_item.set(item).ok();
}

/// Ivars on the `AppDelegate` Objective-C class. Wrapped in interior
/// mutability so the delegate methods can mutate without `&mut self`.
#[derive(Default)]
pub struct AppDelegateIvars {
    queue: std::sync::OnceLock<Arc<PendingQueue>>,
    status_item: std::sync::OnceLock<Retained<NSStatusItem>>,
}

define_class!(
    // SAFETY: superclass NSObject has no subclass requirements;
    // AppDelegate does not implement Drop and stores only Send/Sync
    // ivars (Arc, OnceLock).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VetterAppDelegate"]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: `NSApplicationDelegate` has no extra safety requirements.
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            install_notification_machinery(mtm, self);
            install_status_item(mtm, self.ivars());
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

            let decision = if action == ACTION_APPROVE {
                Some(PendingDecision::allow("approved via notification"))
            } else if action == ACTION_REJECT {
                Some(PendingDecision::deny("rejected via notification"))
            } else if action == dismiss {
                Some(PendingDecision::deny(
                    "dismissed via notification (treated as reject)",
                ))
            } else if action == default {
                // User clicked the body of the banner without picking
                // an action. We treat this as a non-decision and rely
                // on the eventual shutdown / cancel_all to deny so
                // the agent doesn't hang. Logging makes the
                // ambiguity visible.
                eprintln!(
                    "vetterd: notification id `{id}` clicked through; \
                     awaiting explicit Approve/Reject"
                );
                None
            } else {
                eprintln!("vetterd: unknown action `{action}` for id `{id}`");
                None
            };

            if let Some(d) = decision {
                if let Some(queue) = self.ivars().queue.get() {
                    queue.resolve(&id, d);
                }
            }

            completion_handler.call(());
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, queue: Arc<PendingQueue>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        let this: Retained<Self> = unsafe { objc2::msg_send![super(this), init] };
        this.ivars().queue.set(queue).ok();
        this
    }
}
