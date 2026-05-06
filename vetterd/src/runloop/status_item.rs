//! Menu-bar status item for the Vetter approver.
//!
//! Wraps an `NSStatusItem` with the Vetter shield template icon plus
//! a pending-count badge. The button's action is wired to the
//! `togglePopover:` selector on the [`super::AppDelegate`], so a
//! click anywhere on the icon shows / hides the review popover.
//!
//! ## Icon source
//!
//! The default icon is the bundled `Contents/Resources/StatusItem.png`
//! template image (monochrome shield + check, transparent background;
//! built from `assets/vetter-logo-mono.svg` by `tools/build-icons.sh`).
//! Loaded once at install time via
//! [`NSBundle::mainBundle.pathForResource:ofType:`] so the popover and
//! the Finder icon stay visually unified.
//!
//! When the bundle resource is missing (the daemon is being run
//! outside `Vetter.app` — `cargo run` against the `noop` notifier,
//! the test-only mock notifier, etc.) we fall back to Apple's
//! `checkmark.shield` SF Symbol. The fallback keeps the menu-bar UI
//! usable in dev runs without forcing every contributor to rebuild
//! the icon assets to bring the daemon up.
//!
//! The title beside the icon is used as a count badge: empty when
//! nothing is pending, ` N` when N requests are waiting. Plain
//! text instead of a circular badge view keeps the cell layout
//! standard (no custom `NSStatusBarButton` subclass), which matters
//! because clicking elsewhere on a non-standard button-cell can
//! eat the `togglePopover:` selector.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2::AnyThread;
use objc2_app_kit::{
    NSImage, NSStatusBar, NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSBundle, NSSize, NSString};

use super::AppDelegate;

/// Bundled menu-bar template image. Lives at
/// `Vetter.app/Contents/Resources/StatusItem.png` after
/// [`crate::tools::build_icons`] / `tools/_bundle_layout.sh` have
/// run. Rendered into the status-bar button via
/// [`apply_image`] with `setTemplate(true)`, so AppKit tints it
/// light/dark to match the menu bar appearance.
const BUNDLE_IMAGE_NAME: &str = "StatusItem";
const BUNDLE_IMAGE_TYPE: &str = "png";

/// Logical point size requested for the menu-bar icon. The system
/// menu bar is ~22pt tall on standard density and the cell adds a
/// few points of vertical padding, so 18×18pt fills the available
/// glyph slot without crowding the system separators.
///
/// AppKit downsamples our 256×256 source PNG (and its retina
/// representations) to this size when drawing; a single high-res
/// representation is enough because the system already knows how
/// to scale a template image cleanly.
const ICON_POINT_SIZE: f64 = 18.0;

/// SF Symbol used when the bundled template image is missing
/// (non-bundle dev runs, smoke tests, etc.). `checkmark.shield`
/// reads as "guard" and ships in macOS 11+.
const FALLBACK_SYMBOL: &str = "checkmark.shield";

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
            apply_icon(&button);
            button.setTitle(ns_string!(""));
        }

        Self { item }
    }

    /// Update the badge to reflect `count` pending requests. Called
    /// from the queue's change listener via the main-thread hop in
    /// [`super::AppDelegate::refresh_ui`].
    ///
    /// The icon image itself never changes — we communicate
    /// "something is pending" with the trailing count badge so the
    /// shield silhouette stays the same regardless of state. An
    /// earlier pass swapped between filled and unfilled SF Symbol
    /// variants here; once we moved to the bundled brand template
    /// there is only one source-of-truth glyph and the count is
    /// strictly additive information.
    pub fn set_pending_count(&self, count: usize) {
        let mtm = MainThreadMarker::new()
            .expect("StatusItem::set_pending_count must be called on the main thread");
        let Some(button) = self.item.button(mtm) else {
            return;
        };
        if count == 0 {
            button.setTitle(ns_string!(""));
        } else {
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

/// Stamp the menu-bar icon onto `button`. Tries the bundled
/// `Contents/Resources/StatusItem.png` template first; falls back to
/// the SF Symbol when the bundle resource is unavailable (non-bundle
/// dev runs, future Resources/-stripping packagers, etc.). Both
/// branches mark the image as a template so AppKit can do the
/// dark/light tint.
fn apply_icon(button: &NSStatusBarButton) {
    let descr = NSString::from_str("Vetter approver");
    let img = load_bundle_template_image(&descr).or_else(|| load_fallback_symbol(&descr));
    if let Some(img) = img {
        button.setImage(Some(&img));
    }
}

/// Load `Contents/Resources/StatusItem.png` from the running bundle
/// (if any) and configure it for use as a menu-bar template image.
/// Returns `None` when:
///
/// * `mainBundle.pathForResource:ofType:` returns nil (the daemon is
///   not running inside an `.app`).
/// * `NSImage::initWithContentsOfFile:` fails to decode the file
///   (unlikely, but better to fall back than ship a broken icon).
fn load_bundle_template_image(descr: &NSString) -> Option<Retained<NSImage>> {
    let bundle = NSBundle::mainBundle();
    let path = bundle.pathForResource_ofType(
        Some(&NSString::from_str(BUNDLE_IMAGE_NAME)),
        Some(&NSString::from_str(BUNDLE_IMAGE_TYPE)),
    )?;
    let img = NSImage::initWithContentsOfFile(NSImage::alloc(), &path)?;
    img.setTemplate(true);
    img.setSize(NSSize::new(ICON_POINT_SIZE, ICON_POINT_SIZE));
    img.setAccessibilityDescription(Some(descr));
    Some(img)
}

/// SF Symbol fallback when no bundled template image is reachable.
/// Used by smoke-tests / dev runs where the daemon executable is
/// not under `Vetter.app/Contents/MacOS/`.
fn load_fallback_symbol(descr: &NSString) -> Option<Retained<NSImage>> {
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(FALLBACK_SYMBOL),
        Some(descr),
    )?;
    img.setTemplate(true);
    Some(img)
}
