//! Review popover anchored to the menu-bar status item.
//!
//! Lists every currently-pending request as a card with its §8.5
//! detail and per-card **Approve** / **Reject** buttons. Clicking a
//! button calls [`crate::runloop::resolve`] which resolves the
//! matching id in the [`PendingQueue`] **and** removes any delivered
//! notification banner so the user doesn't see a stale card sitting
//! in Notification Center.
//!
//! The popover is opened from two places:
//!
//! - [`super::AppDelegate::toggle_popover`], wired to the status-item
//!   button's `togglePopover:` action.
//! - [`super::AppDelegate::show_popover_anchored`], called from the
//!   notification-action handler when the user taps the banner body
//!   (the "default action") instead of an Approve/Reject button.
//!
//! Card buttons identify themselves via `setTag(idx)` where `idx` is
//! the position of the corresponding id in the controller's
//! `card_ids` vector. `refresh` rebuilds both — the tags stay in
//! lockstep with the underlying queue snapshot for that paint.

use std::sync::{Arc, Mutex};

use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2::DefinedClass;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBezelStyle, NSButton, NSColor, NSFont, NSPopover, NSPopoverBehavior, NSPopoverDelegate,
    NSScrollView, NSStackView, NSStackViewDistribution, NSStatusBarButton, NSTextField, NSTextView,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize,
    NSString,
};

use super::AppDelegate;
use crate::pending::{PendingQueue, PromptSummary};

/// Outer popover dimensions. Width is fixed; height grows with
/// content up to this cap, then the inner scroll view scrolls.
const POPOVER_WIDTH: f64 = 480.0;
const POPOVER_HEIGHT: f64 = 500.0;
const CARD_SPACING: f64 = 12.0;
const CARD_PADDING: f64 = 12.0;
const DETAIL_HEIGHT: f64 = 180.0;

/// `NSBezelStyle::Rounded` is the historic name used by AppKit
/// docs but is now flagged deprecated in favour of the literal
/// `NSBezelStyle::Push` (same value, just renamed). Pin our use to
/// the supported constant so the build stays warning-clean.
const BEZEL_ROUNDED: NSBezelStyle = NSBezelStyle::Push;

/// Public handle the [`super::AppDelegate`] keeps in its ivars. Owns
/// the popover and the controller that backs the per-card button
/// targets. Cheap to clone (everything is `Retained` / `Arc`).
pub struct Popover {
    popover: Retained<NSPopover>,
    controller: Retained<PopoverController>,
}

impl Popover {
    pub fn new(mtm: MainThreadMarker, queue: Arc<PendingQueue>, delegate: &AppDelegate) -> Self {
        // Build the cards stack first; the controller borrows a
        // strong ref to it so refresh can rebuild children.
        let cards = NSStackView::new(mtm);
        cards.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        cards.setSpacing(CARD_SPACING);
        cards.setDistribution(NSStackViewDistribution::Fill);

        // Wrap the cards in a scroll view so a popover with N>1
        // pending requests doesn't grow off-screen.
        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setDocumentView(Some(&cards));
        scroll.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(POPOVER_WIDTH, POPOVER_HEIGHT),
        ));

        // The popover's content controller wraps a container view
        // that hosts the scroll view and a fixed footer (Quit
        // button). Width is pinned by the popover's contentSize.
        let container = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(POPOVER_WIDTH, POPOVER_HEIGHT),
            ),
        );
        scroll.setFrame(NSRect::new(
            NSPoint::new(0.0, 36.0),
            NSSize::new(POPOVER_WIDTH, POPOVER_HEIGHT - 36.0),
        ));
        container.addSubview(&scroll);

        // Footer with a Quit button on the right. Replaces the
        // PR-1 `NSStatusItem.menu` Quit entry now that the button
        // routes clicks to `togglePopover:` instead of opening a menu.
        //
        // Targets `requestShutdown:` on the AppDelegate, **not**
        // `terminate:` on NSApp. `terminate:` calls `exit()` after
        // its delegate ceremony and would skip the cleanup tail in
        // `vetterd::run` that removes the socket and pidfile.
        // `requestShutdown:` flips the shared shutdown atomic which
        // the observer thread in `runloop::run_app_kit` translates
        // into a graceful `[NSApp stop:]`.
        let delegate_obj: &AnyObject =
            unsafe { &*(delegate as *const AppDelegate).cast::<AnyObject>() };
        let quit = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Quit Vetter"),
                Some(delegate_obj),
                Some(sel!(requestShutdown:)),
                mtm,
            )
        };
        quit.setBezelStyle(BEZEL_ROUNDED);
        quit.setFrame(NSRect::new(
            NSPoint::new(POPOVER_WIDTH - 110.0 - 12.0, 6.0),
            NSSize::new(110.0, 28.0),
        ));
        container.addSubview(&quit);

        let vc = NSViewController::new(mtm);
        vc.setView(&container);

        // Construct the controller before creating the popover so
        // we can install it as the popover's delegate (for the
        // popoverWillShow: refresh hook).
        let controller = PopoverController::new(mtm, queue, cards);

        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover.setContentSize(NSSize::new(POPOVER_WIDTH, POPOVER_HEIGHT));
        popover.setContentViewController(Some(&vc));

        let proto = objc2::runtime::ProtocolObject::from_ref(&*controller);
        popover.setDelegate(Some(proto));

        Self {
            popover,
            controller,
        }
    }

    /// Rebuild the card list from `entries`. If `focused_id` is
    /// `Some`, scroll the matching card into view after the layout
    /// settles — used by the notification click-through path so the
    /// banner the user just tapped is the one they see first when
    /// the popover opens. `None` means leave the scroll position
    /// alone (the queue change-listener path uses this so the
    /// popover doesn't bounce while the user is reading).
    pub fn refresh(&self, entries: &[(PromptSummary, String)], focused_id: Option<&str>) {
        self.controller.refresh(entries, focused_id);
    }

    pub fn is_shown(&self) -> bool {
        self.popover.isShown()
    }

    pub fn close(&self) {
        // performClose animates closed and notifies the delegate;
        // safe to call when already hidden (it's a no-op).
        unsafe { self.popover.performClose(None) };
    }

    pub fn show_relative_to(&self, anchor: Option<Retained<NSStatusBarButton>>) {
        let Some(anchor) = anchor else {
            eprintln!("vetterd: popover requested with no status-item button");
            return;
        };
        if self.popover.isShown() {
            return;
        }
        let bounds = anchor.bounds();
        self.popover.showRelativeToRect_ofView_preferredEdge(
            bounds,
            anchor.as_ref(),
            NSRectEdge::MinY,
        );
    }
}

/// Ivars on the popover's Obj-C controller. Card identifiers are
/// stored in lockstep with the per-card buttons' `tag` values; the
/// approve/reject selectors read `[sender tag]` and look up the id
/// here.
#[derive(Default)]
pub struct PopoverControllerIvars {
    queue: std::sync::OnceLock<Arc<PendingQueue>>,
    cards: std::sync::OnceLock<Retained<NSStackView>>,
    /// Ids of currently-rendered cards, indexed by button tag.
    card_ids: Mutex<Vec<String>>,
}

define_class!(
    // SAFETY: superclass NSObject has no subclass requirements;
    // PopoverController stores only Send+Sync ivars and is
    // accessed exclusively on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VetterPopoverController"]
    #[ivars = PopoverControllerIvars]
    pub struct PopoverController;

    unsafe impl NSObjectProtocol for PopoverController {}

    // SAFETY: NSPopoverDelegate has no extra safety requirements;
    // the only method we implement is `popoverWillShow:` which
    // matches the protocol signature exactly.
    unsafe impl NSPopoverDelegate for PopoverController {
        #[unsafe(method(popoverWillShow:))]
        fn popover_will_show(&self, _notification: &objc2_foundation::NSNotification) {
            // Refresh from the queue snapshot so the popover is
            // always opened with current data — even if the user
            // approved a banner just before clicking the icon.
            // No focused id here: this fires on *every* show, and
            // the click-through path has already done its own
            // refresh-with-focus before triggering the show.
            let entries = self.ivars().queue().pending_entries();
            self.refresh(&entries, None);
        }
    }

    impl PopoverController {
        /// Approve button selector. `sender` is the NSButton; its
        /// `tag` indexes into `card_ids` from the most recent
        /// refresh.
        #[unsafe(method(approveClicked:))]
        fn approve_clicked(&self, sender: Option<&NSButton>) {
            self.resolve_for_tag(sender, true);
        }

        /// Reject button selector. Same id-lookup contract as Approve.
        #[unsafe(method(rejectClicked:))]
        fn reject_clicked(&self, sender: Option<&NSButton>) {
            self.resolve_for_tag(sender, false);
        }
    }
);

impl PopoverController {
    fn new(
        mtm: MainThreadMarker,
        queue: Arc<PendingQueue>,
        cards: Retained<NSStackView>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PopoverControllerIvars::default());
        let this: Retained<Self> = unsafe { objc2::msg_send![super(this), init] };
        this.ivars().queue.set(queue).ok();
        this.ivars().cards.set(cards).ok();
        this
    }

    fn ivars_queue(&self) -> &Arc<PendingQueue> {
        self.ivars().queue()
    }

    /// Borrow the cards stack. Always returns Some after `new`.
    fn cards(&self) -> &Retained<NSStackView> {
        self.ivars()
            .cards
            .get()
            .expect("cards stack is set in PopoverController::new")
    }

    fn resolve_for_tag(&self, sender: Option<&NSButton>, allow: bool) {
        let Some(button) = sender else { return };
        let tag = button.tag();
        let id = {
            let g = self.ivars().card_ids.lock().expect("card_ids poisoned");
            g.get(tag as usize).cloned()
        };
        let Some(id) = id else {
            eprintln!("vetterd: popover button tag {tag} out of range");
            return;
        };
        super::resolve(self.ivars_queue(), &id, allow);
    }

    /// Replace the cards stack with one card per pending entry.
    /// If `focused_id` is `Some` and matches an entry, that card's
    /// view is scrolled into view after the layout pass.
    /// Runs on the main thread.
    fn refresh(&self, entries: &[(PromptSummary, String)], focused_id: Option<&str>) {
        let mtm = MainThreadMarker::new()
            .expect("PopoverController::refresh must be called on the main thread");

        let cards = self.cards();

        // Clear existing cards. `arrangedSubviews()` returns a
        // snapshot NSArray (not a live proxy), so iterating it
        // while we mutate the stack view is safe.
        //
        // `[NSView removeFromSuperview]` already removes the view
        // from both `subviews` and `arrangedSubviews`. Calling
        // `removeArrangedSubview:` afterwards would hit an
        // `NSAssertionHandler` failure inside
        // `_removeView:animated:removeFromViewHierarchy:` because
        // the view is no longer in the arranged list — it crashed
        // the daemon on the second refresh in real-app testing.
        let existing = cards.arrangedSubviews();
        let count = existing.count();
        for i in 0..count {
            existing.objectAtIndex(i).removeFromSuperview();
        }

        // Update tags-id mapping atomically before we install the
        // new buttons.
        {
            let mut g = self.ivars().card_ids.lock().expect("card_ids poisoned");
            g.clear();
            g.extend(entries.iter().map(|(s, _)| s.id.clone()));
        }

        if entries.is_empty() {
            let empty = NSTextField::labelWithString(ns_string!("No pending requests."), mtm);
            empty.setAlignment(objc2_app_kit::NSTextAlignment::Center);
            cards.addArrangedSubview(&empty);
            return;
        }

        let mut focused_view: Option<Retained<NSView>> = None;
        for (idx, (summary, rendered)) in entries.iter().enumerate() {
            let card = self.build_card(mtm, idx, summary, rendered);
            if focused_id.is_some_and(|f| f == summary.id) {
                focused_view = Some(card.clone());
            }
            cards.addArrangedSubview(&card);
        }

        // Trigger a layout pass so frame data is current, then ask
        // the focused card to scroll into view. AppKit's scrollToView
        // operates on the enclosing scroll view automatically.
        if let Some(view) = focused_view {
            cards.layoutSubtreeIfNeeded();
            let bounds = view.bounds();
            view.scrollRectToVisible(bounds);
        }
    }

    fn build_card(
        &self,
        mtm: MainThreadMarker,
        idx: usize,
        summary: &PromptSummary,
        rendered: &str,
    ) -> Retained<NSView> {
        // Header: "<command> <verb> <target>" with a "dry run" pill
        // for force_prompt requests.
        let header_text = if summary.primary_verb.is_empty() {
            format!("{}  {}", summary.command, summary.primary_target)
        } else {
            format!(
                "{}  {} {}",
                summary.command, summary.primary_verb, summary.primary_target
            )
        };
        let header = NSTextField::labelWithString(&NSString::from_str(&header_text), mtm);
        header.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        let dry = if summary.force_prompt {
            let pill = NSTextField::labelWithString(ns_string!("dry run"), mtm);
            pill.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            pill.setTextColor(Some(&NSColor::secondaryLabelColor()));
            Some(pill)
        } else {
            None
        };

        let header_row = NSStackView::new(mtm);
        header_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        header_row.setSpacing(8.0);
        header_row.setDistribution(NSStackViewDistribution::Fill);
        header_row.addArrangedSubview(&header);
        if let Some(p) = &dry {
            header_row.addArrangedSubview(p);
        }

        // Body: read-only NSTextView in its own NSScrollView. The
        // canonical AppKit incantation `+scrollableTextView` wires
        // up the document view + scrollers in one go.
        let body_scroll = NSTextView::scrollableTextView(mtm);
        body_scroll.setHasVerticalScroller(true);
        body_scroll.setHasHorizontalScroller(false);
        body_scroll.setBorderType(objc2_app_kit::NSBorderType::LineBorder);
        body_scroll.setDrawsBackground(true);
        body_scroll.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(POPOVER_WIDTH - CARD_PADDING * 2.0, DETAIL_HEIGHT),
        ));
        if let Some(doc) = body_scroll.documentView() {
            // The document view returned from `scrollableTextView`
            // is an NSTextView; cast and configure as monospaced
            // read-only.
            let tv: Retained<NSTextView> = unsafe { Retained::cast_unchecked(doc) };
            tv.setEditable(false);
            tv.setSelectable(true);
            tv.setRichText(false);
            tv.setDrawsBackground(true);
            tv.setBackgroundColor(&NSColor::textBackgroundColor());
            let font =
                NSFont::userFixedPitchFontOfSize(11.0).unwrap_or(NSFont::systemFontOfSize(11.0));
            tv.setFont(Some(&font));
            tv.setString(&NSString::from_str(rendered));
        }

        // Approve / Reject row.
        let approve = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Approve"),
                Some(&*(self as *const Self).cast::<AnyObject>()),
                Some(sel!(approveClicked:)),
                mtm,
            )
        };
        approve.setBezelStyle(BEZEL_ROUNDED);
        approve.setTag(idx as isize);
        let reject = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Reject"),
                Some(&*(self as *const Self).cast::<AnyObject>()),
                Some(sel!(rejectClicked:)),
                mtm,
            )
        };
        reject.setBezelStyle(BEZEL_ROUNDED);
        reject.setTag(idx as isize);

        let buttons = NSStackView::new(mtm);
        buttons.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        buttons.setSpacing(8.0);
        buttons.setDistribution(NSStackViewDistribution::Fill);
        buttons.addArrangedSubview(&reject);
        buttons.addArrangedSubview(&approve);

        // Card container: vertical stack with header / body / buttons.
        let card = NSStackView::new(mtm);
        card.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        card.setSpacing(8.0);
        card.setDistribution(NSStackViewDistribution::Fill);
        card.addArrangedSubview(&header_row);
        card.addArrangedSubview(&body_scroll);
        card.addArrangedSubview(&buttons);

        // Coerce the stack view into a plain NSView for return.
        let view: Retained<NSView> = card.into_super();
        view
    }
}

trait PopoverIvarsAccess {
    fn queue(&self) -> &Arc<PendingQueue>;
}

impl PopoverIvarsAccess for PopoverControllerIvars {
    fn queue(&self) -> &Arc<PendingQueue> {
        self.queue
            .get()
            .expect("queue set in PopoverController::new")
    }
}
