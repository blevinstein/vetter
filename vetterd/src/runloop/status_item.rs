//! Menu-bar status item for the Vetter approver.
//!
//! Wraps an `NSStatusItem` with a templated SF Symbol icon plus a
//! pending-count badge. The button's action is wired to the
//! `togglePopover:` selector on the [`super::AppDelegate`], so a
//! click anywhere on the icon shows / hides the review popover.
//!
//! The title beside the icon is used as a count badge: empty when
//! nothing is pending, ` (N)` when N requests are waiting. Plain
//! text instead of a circular badge view keeps the cell layout
//! standard (no custom `NSStatusBarButton` subclass), which matters
//! because clicking elsewhere on a non-standard button-cell can
//! eat the `togglePopover:` selector.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2_app_kit::{
    NSImage, NSStatusBar, NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSString};

use super::AppDelegate;

/// SF Symbol used as the menu-bar icon. `checkmark.shield` reads as
/// "guard" while still being unambiguously distinct from system
/// items. Filled variant signals "you have something to look at".
const SYMBOL_IDLE: &str = "checkmark.shield";
const SYMBOL_PENDING: &str = "checkmark.shield.fill";

/// Owner type for the status item. Holds the `Retained<NSStatusItem>`
/// so the system doesn't garbage-collect it (an unanchored
/// `NSStatusItem` is silently released and disappears from the bar).
pub struct StatusItem {
    item: Retained<NSStatusItem>,
}

impl StatusItem {
    /// Install the status item, wire its button to `delegate`'s
    /// `togglePopover:` selector, and load the idle icon.
    pub fn install(mtm: MainThreadMarker, delegate: &AppDelegate) -> Self {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);

        if let Some(button) = item.button(mtm) {
            // SAFETY: the AppDelegate is retained by NSApplication
            // for the lifetime of the process, so the target
            // pointer stays valid until we tear NSApp down.
            // `togglePopover:` exists on AppDelegate (defined via
            // `define_class!`) and takes a single id sender.
            unsafe {
                let target: &AnyObject = &*(delegate as *const AppDelegate).cast::<AnyObject>();
                button.setTarget(Some(target));
                button.setAction(Some(sel!(togglePopover:)));
            }
            // Use the symbol image; setTemplate(true) makes the
            // system tint it for light/dark menu bars. If the
            // symbol isn't available (running on a pre-Big Sur
            // OS — extremely unlikely, since AppKit minimum here
            // is 11+) we fall back to a text title so the user
            // can still find the daemon.
            apply_image(&button, SYMBOL_IDLE);
            button.setTitle(ns_string!(""));
        }

        Self { item }
    }

    /// Update the badge to reflect `count` pending requests. Called
    /// from the queue's change listener via the main-thread hop in
    /// [`super::AppDelegate::refresh_ui`].
    pub fn set_pending_count(&self, count: usize) {
        let mtm = MainThreadMarker::new()
            .expect("StatusItem::set_pending_count must be called on the main thread");
        let Some(button) = self.item.button(mtm) else {
            return;
        };
        if count == 0 {
            apply_image(&button, SYMBOL_IDLE);
            button.setTitle(ns_string!(""));
        } else {
            apply_image(&button, SYMBOL_PENDING);
            // Leading space separates the badge from the icon.
            let title = NSString::from_str(&format!(" {count}"));
            button.setTitle(&title);
        }
    }

    /// Borrowed reference to the status-bar button. The popover
    /// uses this as its anchor view in
    /// `showRelativeToRect:ofView:preferredEdge:`.
    pub fn button(&self) -> Option<Retained<NSStatusBarButton>> {
        let mtm =
            MainThreadMarker::new().expect("StatusItem::button must be called on the main thread");
        self.item.button(mtm)
    }
}

fn apply_image(button: &NSStatusBarButton, symbol: &str) {
    let descr = NSString::from_str("Vetter pending requests");
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&descr),
    );
    if let Some(img) = img {
        img.setTemplate(true);
        button.setImage(Some(&img));
    }
}
