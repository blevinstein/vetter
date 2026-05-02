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
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBezelStyle, NSBox,
    NSBoxType, NSButton, NSButtonType, NSColor, NSControlStateValueOff, NSFont,
    NSFontWeightSemibold, NSLayoutConstraint, NSPopover, NSPopoverBehavior, NSPopoverDelegate,
    NSScrollView, NSStackView, NSStackViewDistribution, NSStatusBarButton, NSTextField, NSTextView,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSEdgeInsets, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSRectEdge, NSSize, NSString,
};

use super::{popover_attr, popover_effects, popover_pills, popover_url, AppDelegate};
use crate::pending::{PendingQueue, PromptSummary, ResolvedEntry};

/// Outer popover dimensions. Width is fixed; height grows with
/// content up to this cap, then the inner scroll view scrolls.
///
/// Bumped from 480 → 560 once the URL row + sorted pills row + per-
/// effect rows started competing for horizontal real estate. The
/// older width forced long URLs to truncate and crowded the
/// pills row whenever a request emitted three or more pills.
const POPOVER_WIDTH: f64 = 560.0;
const POPOVER_HEIGHT: f64 = 500.0;
/// Horizontal breathing room between the popover's content edge and
/// the cards stack. Combined with each card's `CARD_INSET` (which
/// pads content *inside* the card chrome), this gives ~24pt of
/// total margin from the popover edge to any text — enough that
/// long URLs don't visually butt against the rounded popover
/// corner. Vertical margins remain 0 so the cards stack hugs the
/// scroll view's clip edges as before.
const CARDS_HORIZONTAL_MARGIN: f64 = 12.0;
/// Outer gap between adjacent cards in the cards stack. Bumped from
/// 12 → 16 once we started inserting `NSBoxType::Separator` lines
/// between cards: the rule line wants a bit more breathing room
/// either side or it visually crowds the buttons.
const CARD_SPACING: f64 = 16.0;
/// Inset around each card's content (header / body / buttons). Pulls
/// the body away from any wrapping `NSBox` border (dry-run case) and
/// gives non-dry-run cards consistent breathing room without needing
/// a wrapper view.
const CARD_INSET: f64 = 12.0;
/// Upper bound for the "Show raw" body. The body sizes itself to the
/// rendered text (see `measure_raw_body_height`); this cap kicks in
/// only when the command is unusually long (giant `--data-binary`
/// payload, hundreds of headers, etc.) so a single huge card can't
/// dominate the popover.
const RAW_BODY_MAX_HEIGHT: f64 = 180.0;
/// Lower bound for the "Show raw" body. Two lines worth of
/// monospaced 11pt — enough to fit `curl https://…` or a short
/// argv without horizontal scrolling, even when a system font
/// substitution shrinks the line height a hair.
const RAW_BODY_MIN_HEIGHT: f64 = 32.0;

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
        // Inset the arranged cards horizontally so card content
        // doesn't run flush with the popover's rounded edges.
        // Vertical insets stay 0; the cards stack is pinned to the
        // scroll view's clip view top/bottom by autolayout below
        // and we want the first card to sit at the very top of the
        // scroll area (so vertical scrolling feels natural).
        cards.setEdgeInsets(NSEdgeInsets {
            top: 0.0,
            left: CARDS_HORIZONTAL_MARGIN,
            bottom: 0.0,
            right: CARDS_HORIZONTAL_MARGIN,
        });
        // NSStackView defaults this to `false`, but pin it
        // explicitly so the constraints below are the only thing
        // that gives the document view a size — if a future change
        // re-enables autoresizing translation we'd silently get
        // back to a zero-sized stack view (the bug this whole
        // setup is fixing).
        cards.setTranslatesAutoresizingMaskIntoConstraints(false);

        // Wrap the cards in a scroll view so a popover with N>1
        // pending requests doesn't grow off-screen.
        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setDocumentView(Some(&cards));

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

        // Anchor the cards stack inside the scroll view's clip
        // view. Without these constraints `NSStackView` (an Auto
        // Layout view) stays at zero size as the document view
        // and every card silently renders invisible — the popover
        // looks empty even with pending requests in the queue.
        // Pinning width to `scroll.widthAnchor()` (not
        // `contentView.widthAnchor()`) keeps the stack the exact
        // visible width and stops the horizontal scroller ever
        // appearing; height is implicit (sum of arranged subview
        // heights) so the vertical scroller engages once N cards
        // exceed the popover height.
        let clip = scroll.contentView();
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
            cards
                .leadingAnchor()
                .constraintEqualToAnchor(&clip.leadingAnchor()),
            cards
                .trailingAnchor()
                .constraintEqualToAnchor(&clip.trailingAnchor()),
            cards.topAnchor().constraintEqualToAnchor(&clip.topAnchor()),
            cards
                .widthAnchor()
                .constraintEqualToAnchor(&scroll.widthAnchor()),
        ]));

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

        // Force the entire popover (and every child view AppKit
        // resolves through `effectiveAppearance`) into Dark Aqua.
        // The pill colours we picked were calibrated on a dark
        // surface — running them against a light-mode popover made
        // the orange / yellow text hard to read regardless of pill
        // background. Pinning the appearance gives the same look
        // for users on Light, Dark, and Auto system themes.
        if let Some(dark) = unsafe { NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua) } {
            popover.setAppearance(Some(&dark));
            container.setAppearance(Some(&dark));
        }

        let proto = objc2::runtime::ProtocolObject::from_ref(&*controller);
        popover.setDelegate(Some(proto));

        Self {
            popover,
            controller,
        }
    }

    /// Rebuild the card list from `entries` (pending) and `resolved`
    /// (recently decided). Pending cards appear first with
    /// Approve/Reject buttons; resolved cards follow with an outcome
    /// badge and no action buttons.
    ///
    /// If `focused_id` is `Some`, scroll the matching pending card
    /// into view after the layout settles — used by the notification
    /// click-through path so the banner the user just tapped is the
    /// one they see first. `None` leaves the scroll position alone.
    pub fn refresh(
        &self,
        entries: &[(PromptSummary, String)],
        resolved: &[ResolvedEntry],
        focused_id: Option<&str>,
    ) {
        self.controller.refresh(entries, resolved, focused_id);
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
/// here. The `body_scrolls` vec is the same idea for the per-card
/// "Show raw" disclosure: each pending card's body `NSScrollView`
/// is parked at index `tag` so the disclosure action can flip its
/// `setHidden` state without us having to construct a new
/// closure-bearing target object per card.
#[derive(Default)]
pub struct PopoverControllerIvars {
    queue: std::sync::OnceLock<Arc<PendingQueue>>,
    cards: std::sync::OnceLock<Retained<NSStackView>>,
    /// Ids of currently-rendered cards, indexed by button tag.
    card_ids: Mutex<Vec<String>>,
    /// Per-card raw-body `NSScrollView`s, indexed by `disclosure_idx`
    /// (the global counter `refresh` maintains across pending and
    /// resolved cards). The "Show raw" disclosure action toggles
    /// `setHidden` on the matching entry. Cleared alongside
    /// `card_ids` on every refresh.
    body_scrolls: Mutex<Vec<Retained<NSScrollView>>>,
    /// Per-card structured-effects container views, indexed by
    /// `disclosure_idx` in the same way as `body_scrolls`. Pending
    /// cards always render their effect rows directly into the
    /// card and store `None` here — the "Details" disclosure is
    /// resolved-only because pending cards need the structured
    /// info visible so the user can decide whether to approve.
    /// Resolved cards store `Some(container)` and the
    /// `toggleDetailsDisclosure:` selector flips `setHidden` on it.
    details_views: Mutex<Vec<Option<Retained<NSView>>>>,
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
            let (entries, resolved) = self.ivars().queue().all_entries();
            self.refresh(&entries, &resolved, None);
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

        /// "Show raw" disclosure selector. Reads `sender.state` to
        /// decide whether to unhide (state == On) or re-hide
        /// (state == Off) the body `NSScrollView` parked at
        /// `body_scrolls[sender.tag]`. We update *only* the matching
        /// scroll view; the whole-popover refresh path doesn't call
        /// us, so disclosure state is intentionally per-card.
        #[unsafe(method(toggleRawDisclosure:))]
        fn toggle_raw_disclosure(&self, sender: Option<&NSButton>) {
            self.toggle_disclosure_for_tag(sender);
        }

        /// "Details" disclosure selector for resolved cards. Same
        /// per-tag, per-card pattern as `toggleRawDisclosure:` but
        /// targets the structured-effects container parked at
        /// `details_views[sender.tag]`. Pending cards never wire
        /// up this button (their effect rows are always visible),
        /// so the matching slot is `None` and a stray firing is a
        /// no-op rather than a panic.
        #[unsafe(method(toggleDetailsDisclosure:))]
        fn toggle_details_disclosure(&self, sender: Option<&NSButton>) {
            self.toggle_details_for_tag(sender);
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

    /// Toggle visibility of the raw-body `NSScrollView` parked at
    /// `body_scrolls[sender.tag]`. The disclosure button is a
    /// `PushOnPushOff`-typed `NSButton`; AppKit flips its state
    /// before our action fires, so we just mirror it onto the
    /// scroll view's hidden flag and rotate the chevron in the
    /// title text to track the new state.
    fn toggle_disclosure_for_tag(&self, sender: Option<&NSButton>) {
        let Some(button) = sender else { return };
        let tag = button.tag();
        let off = NSControlStateValueOff;
        let hide = button.state() == off;
        let scroll = {
            let g = self
                .ivars()
                .body_scrolls
                .lock()
                .expect("body_scrolls poisoned");
            g.get(tag as usize).cloned()
        };
        if let Some(scroll) = scroll {
            scroll.setHidden(hide);
            // Rotate the chevron prefix so the button title tracks
            // the disclosure state. `▸` for collapsed, `▾` for
            // expanded — same convention as `NSOutlineView`.
            let title = if hide { "▸ Show raw" } else { "▾ Hide raw" };
            button.setTitle(&NSString::from_str(title));
        } else {
            eprintln!("vetterd: disclosure button tag {tag} out of range");
        }
    }

    /// Resolved-card sibling of `toggle_disclosure_for_tag`. Toggles
    /// the structured-effects container parked at
    /// `details_views[sender.tag]`. Pending-card slots are `None`
    /// (their effect rows are always visible) so a stray firing is
    /// a no-op rather than a panic — useful when a refresh races
    /// with a click.
    fn toggle_details_for_tag(&self, sender: Option<&NSButton>) {
        let Some(button) = sender else { return };
        let tag = button.tag();
        let off = NSControlStateValueOff;
        let hide = button.state() == off;
        let view = {
            let g = self
                .ivars()
                .details_views
                .lock()
                .expect("details_views poisoned");
            g.get(tag as usize).cloned().flatten()
        };
        if let Some(view) = view {
            view.setHidden(hide);
            let title = if hide {
                "▸ Details"
            } else {
                "▾ Hide details"
            };
            button.setTitle(&NSString::from_str(title));
        }
    }

    /// Replace the cards stack with one card per pending entry
    /// followed by a "Recent" section for resolved entries.
    /// If `focused_id` is `Some` and matches a pending entry, that
    /// card's view is scrolled into view after the layout pass.
    /// Runs on the main thread.
    fn refresh(
        &self,
        entries: &[(PromptSummary, String)],
        resolved: &[ResolvedEntry],
        focused_id: Option<&str>,
    ) {
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

        // Tag-to-id mapping covers only pending cards; resolved cards
        // have no action buttons and therefore no tags.
        {
            let mut g = self.ivars().card_ids.lock().expect("card_ids poisoned");
            g.clear();
            g.extend(entries.iter().map(|(s, _)| s.id.clone()));
        }
        // Reset the disclosure registries; `build_card` will repopulate
        // them (one entry per card, keyed by `disclosure_idx == tag`).
        // `details_views` keeps a slot per card too — pending slots
        // are `None` because pending cards never wire up a Details
        // disclosure (their effect rows are always visible so the
        // user can decide). Keeping the two registries in lockstep
        // makes the per-tag lookup symmetric across both selectors.
        self.ivars()
            .body_scrolls
            .lock()
            .expect("body_scrolls poisoned")
            .clear();
        self.ivars()
            .details_views
            .lock()
            .expect("details_views poisoned")
            .clear();

        if entries.is_empty() && resolved.is_empty() {
            let empty = NSTextField::labelWithString(ns_string!("No pending requests."), mtm);
            empty.setAlignment(objc2_app_kit::NSTextAlignment::Center);
            cards.addArrangedSubview(&empty);
            return;
        }

        // `disclosure_idx` is a *global* counter spanning both the
        // pending and resolved sections — it indexes into the
        // controller's `body_scrolls` registry so the per-card
        // "Show raw" toggle action can find the right scroll view
        // regardless of which section the card lives in. The
        // per-section `idx` is still passed separately because
        // pending cards' Approve/Reject buttons key off it.
        let mut disclosure_idx = 0_usize;
        let mut focused_view: Option<Retained<NSView>> = None;
        for (idx, (summary, rendered)) in entries.iter().enumerate() {
            // Thin horizontal rule between adjacent cards.
            if idx > 0 {
                let sep = NSBox::new(mtm);
                sep.setBoxType(NSBoxType::Separator);
                cards.addArrangedSubview(&sep);
            }
            let card = self.build_card(mtm, idx, disclosure_idx, summary, rendered, None);
            disclosure_idx += 1;
            if focused_id.is_some_and(|f| f == summary.id) {
                focused_view = Some(card.clone());
            }
            cards.addArrangedSubview(&card);
        }

        if !resolved.is_empty() {
            // "Recent" header sits between the pending block and the
            // resolved block (or at the very top when nothing is
            // pending). Drop a separator before it whenever pending
            // cards precede it so the two sections read distinctly.
            if !entries.is_empty() {
                let sep = NSBox::new(mtm);
                sep.setBoxType(NSBoxType::Separator);
                cards.addArrangedSubview(&sep);
            }
            let section_label = NSTextField::labelWithString(ns_string!("Recent"), mtm);
            section_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            section_label.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
            cards.addArrangedSubview(&section_label);

            for (idx, entry) in resolved.iter().enumerate() {
                let sep = NSBox::new(mtm);
                sep.setBoxType(NSBoxType::Separator);
                cards.addArrangedSubview(&sep);
                // `idx` is unused for resolved cards' buttons (the
                // outcome is already final, so they have no
                // Approve/Reject row); we still pass it so
                // `build_card`'s signature stays uniform.
                // `disclosure_idx` keeps climbing so this card's
                // `Show raw` button lands on its own slot in
                // `body_scrolls`.
                let card = self.build_card(
                    mtm,
                    idx,
                    disclosure_idx,
                    &entry.summary,
                    &entry.rendered,
                    Some(&entry.decision),
                );
                disclosure_idx += 1;
                cards.addArrangedSubview(&card);
            }
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

    /// Build a single card view.
    ///
    /// `outcome` distinguishes card types:
    /// - `None` → pending card: Approve/Reject buttons, normal colours.
    /// - `Some(decision)` → resolved card: outcome badge, no buttons,
    ///   secondary-colour header text.
    fn build_card(
        &self,
        mtm: MainThreadMarker,
        idx: usize,
        disclosure_idx: usize,
        summary: &PromptSummary,
        rendered: &str,
        outcome: Option<&vetter_core::wire::WireDecision>,
    ) -> Retained<NSView> {
        // Header row: `<command>  <smart URL row | fallback label>`.
        //
        // The leading `<command>` slug stays a bold monospaced label
        // (so multiple cards line up vertically) and is followed by
        // either the structured URL row from `popover_url` (when the
        // first effect is an HttpRequest) or a fallback `verb target`
        // label (for non-HttpRequest cards / legacy callers without
        // `parsed`). The dry-run cue continues to live on the outer
        // `NSBox` wrapper at the bottom of this function.
        let command_label =
            NSTextField::labelWithString(&NSString::from_str(&summary.command), mtm);
        let semibold = unsafe { NSFontWeightSemibold };
        command_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            13.0, semibold,
        )));
        // Resolved cards use a dimmer header colour to signal that
        // they are informational (no action required).
        if outcome.is_some() {
            command_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }

        // Pick the smart URL row when we have a parsed HttpRequest;
        // otherwise degrade to the plain verb/target label.
        let url_view = first_http_request(summary)
            .map(|(req, idx)| {
                let host_known = summary.host_known.get(idx).copied().unwrap_or(false);
                popover_url::build_url_row(req, host_known, mtm)
            })
            .unwrap_or_else(|| {
                popover_url::build_fallback_row(
                    "",
                    &summary.primary_verb,
                    &summary.primary_target,
                    mtm,
                )
            });

        let header_row = NSStackView::new(mtm);
        header_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        header_row.setSpacing(8.0);
        header_row.setDistribution(NSStackViewDistribution::Fill);
        header_row.addArrangedSubview(&command_label);
        header_row.addArrangedSubview(&url_view);

        // Dry-run pill — small yellow tinted-pill version of the
        // "this is a `vet --dry-run` request" cue. Replaces the
        // older yellow-`NSBox`-around-the-card treatment, which
        // made dry-run cards visually inconsistent with regular
        // ones. Uses the same `build_pill` recipe as the signal
        // pills so the chrome stays uniform.
        if summary.force_prompt {
            let yellow = NSColor::systemYellowColor();
            let pill = popover_pills::build_pill(
                "dry run",
                &yellow,
                &popover_pills::pill_bg_for(&yellow),
                "vet --dry-run: the agent asked for confirmation even though policy would auto-allow",
                mtm,
            );
            header_row.addArrangedSubview(&pill);
        }

        // For resolved cards, append an "Allowed" or "Denied" badge
        // at the trailing end of the header row so the outcome is
        // immediately scannable without reading the body.
        if let Some(decision) = outcome {
            let (badge_text, badge_color) = match decision {
                vetter_core::wire::WireDecision::Allow
                | vetter_core::wire::WireDecision::AllowOnce => {
                    ("Allowed", NSColor::systemGreenColor())
                }
                vetter_core::wire::WireDecision::Deny => ("Denied", NSColor::systemRedColor()),
            };
            let badge = NSTextField::labelWithString(&NSString::from_str(badge_text), mtm);
            badge.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
            badge.setTextColor(Some(&badge_color));
            header_row.addArrangedSubview(&badge);
        }

        // Body: read-only NSTextView in its own NSScrollView. The
        // canonical AppKit incantation `+scrollableTextView` wires
        // up the document view + scrollers in one go.
        let body_scroll = NSTextView::scrollableTextView(mtm);
        body_scroll.setHasVerticalScroller(true);
        body_scroll.setHasHorizontalScroller(false);
        body_scroll.setBorderType(objc2_app_kit::NSBorderType::LineBorder);
        body_scroll.setDrawsBackground(true);
        // Frame width matters for the text container's wrapping
        // measurement below (see `measure_raw_body_height`); the
        // height is provisional and replaced by an autolayout
        // constraint at the end of `build_card`.
        //
        // Subtract both the cards-stack outer margin and the per-
        // card inset so the wrap column matches what the user will
        // actually see; without the margin term the body
        // measurement was overshooting and the disclosure expanded
        // to a strip wider than its parent card on the new 560pt
        // popover width.
        let body_width = POPOVER_WIDTH - CARDS_HORIZONTAL_MARGIN * 2.0 - CARD_INSET * 2.0;
        body_scroll.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(body_width, RAW_BODY_MIN_HEIGHT),
        ));
        // Measured after the attributed string is installed below;
        // defaults to the minimum so a measurement failure still
        // gives the user something visible to click into.
        let mut content_height = RAW_BODY_MIN_HEIGHT;
        if let Some(doc) = body_scroll.documentView() {
            // The document view returned from `scrollableTextView`
            // is an NSTextView; cast and configure as monospaced
            // read-only. `richText = true` is required for the text
            // storage to honour the per-span colour / font /
            // underline attributes our ANSI translator stamps below;
            // without it the view falls back to its single-font
            // plain-text mode and ignores the NSColor runs.
            let tv: Retained<NSTextView> = unsafe { Retained::cast_unchecked(doc) };
            tv.setEditable(false);
            tv.setSelectable(true);
            tv.setRichText(true);
            tv.setDrawsBackground(true);
            // Pin the text view's appearance to Dark Aqua even
            // though the popover already is. `NSTextView` doesn't
            // always inherit `effectiveAppearance` through the
            // scroll-view → clip-view → document-view chain, so the
            // dynamic system colours below (`textColor`,
            // `textBackgroundColor`) can resolve to the *light*
            // variants and we end up with black text on a dark card.
            // Setting the appearance here makes the resolution
            // unambiguous.
            if let Some(dark) = unsafe { NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua) } {
                body_scroll.setAppearance(Some(&dark));
                tv.setAppearance(Some(&dark));
            }
            tv.setBackgroundColor(&NSColor::textBackgroundColor());
            // Default text colour for un-styled (non-ANSI) runs. The
            // popover's appearance is pinned to Dark Aqua, so
            // `textColor` resolves to a light foreground that reads
            // against the dark `textBackgroundColor` set above.
            // Without this AppKit was rendering plain-text runs in
            // the *light* variant of `textColor` (black) regardless
            // of the popover's pinned appearance — the appearance
            // pin above fixes the dynamic resolution, but stamping
            // an explicit colour is the belt-and-suspenders that
            // keeps it readable even if a future AppKit revision
            // breaks appearance propagation again.
            tv.setTextColor(Some(&NSColor::textColor()));
            let font =
                NSFont::userFixedPitchFontOfSize(11.0).unwrap_or(NSFont::systemFontOfSize(11.0));
            tv.setFont(Some(&font));

            // Translate ANSI SGR escapes from `rendered` into
            // `NSAttributedString` runs so the popover shows the
            // same colour taxonomy as `vet --explain` on a TTY. If
            // the text storage is unexpectedly absent we fall back
            // to the plain string — strictly worse visually but
            // the popover still works.
            let attr = popover_attr::parse_ansi_to_attributed(rendered);
            if let Some(storage) = unsafe { tv.textStorage() } {
                storage.setAttributedString(&attr);
            } else {
                tv.setString(&NSString::from_str(rendered));
            }
            content_height = measure_raw_body_height(&tv, body_width);
        }

        // Inner card container: vertical stack with header / body /
        // (optional) buttons, padded by `CARD_INSET` on every side
        // via `setEdgeInsets`. Insetting here (rather than wrapping
        // in a second container view) keeps the dry-run `NSBox`
        // wrapping cheap — the box just hosts this stack directly.
        let card = NSStackView::new(mtm);
        card.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        card.setSpacing(8.0);
        card.setDistribution(NSStackViewDistribution::Fill);
        card.setEdgeInsets(NSEdgeInsets {
            top: CARD_INSET,
            left: CARD_INSET,
            bottom: CARD_INSET,
            right: CARD_INSET,
        });
        card.addArrangedSubview(&header_row);

        // Pills row — one tinted pill per `Warn`/`Danger`-tier
        // signal kind, deduped on kind. The dedupe rule matters when
        // a request emits the same kind on multiple effects (e.g. a
        // POST + AuthHeader on the same HttpRequest would otherwise
        // paint two `auth-header` pills); we keep the *first*
        // matching `RiskSignal` so its `detail` powers the tooltip.
        // Info-tier kinds (none today) intentionally don't get a
        // pill — that's surfaced inside the "Show raw" disclosure
        // body instead.
        //
        // Order within the row is by triage priority
        // (`popover_pills::signal_priority`): Danger (red) first,
        // Warn (orange) next, AuthHeader (green) last. The user's
        // eye lands on the most-urgent chips before scanning past
        // the supportive ones, regardless of the order the parser
        // emitted the signals in. Ties (two Danger kinds, etc.)
        // keep their first-seen order so the tooltip-detail
        // selection above stays stable.
        //
        // Layout: a horizontal stack placed *below* the header row
        // rather than inside it, so multi-pill cases don't
        // crowd the command/url line. The row is hidden entirely
        // when no pill ends up rendered (no Warn/Danger signals).
        let mut deduped: Vec<&vetter_core::RiskSignal> = Vec::new();
        let mut seen: Vec<vetter_core::SignalKind> = Vec::new();
        for sig in &summary.signals {
            if seen.contains(&sig.kind) {
                continue;
            }
            seen.push(sig.kind);
            deduped.push(sig);
        }
        deduped.sort_by_key(|sig| popover_pills::signal_priority(sig.kind));
        let mut pill_views: Vec<Retained<NSView>> = Vec::new();
        for sig in &deduped {
            if let Some(pill) = popover_pills::build_signal_pill(sig.kind, &sig.detail, mtm) {
                pill_views.push(pill);
            }
        }
        if !pill_views.is_empty() {
            let pills_row = NSStackView::new(mtm);
            pills_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
            pills_row.setSpacing(6.0);
            pills_row.setDistribution(NSStackViewDistribution::Fill);
            for pill in &pill_views {
                pills_row.addArrangedSubview(pill);
            }
            // Trailing flexible spacer so pills bunch left rather
            // than stretching to fill the card width.
            let spacer = NSView::new(mtm);
            pills_row.addArrangedSubview(&spacer);
            card.addArrangedSubview(&pills_row);
        }

        // Per-effect native rows: headers (with redaction), body
        // (typed per `Body` variant), auth, file ops, process
        // spawns. Falls back to nothing when `parsed` is absent
        // (legacy / mock callers); the "Show raw" disclosure below
        // still surfaces the §8.5 layout in that case.
        //
        // Pending vs resolved layout differs:
        // - Pending cards add the rows directly to the card so the
        //   user can scan headers/auth/body before approving. The
        //   matching `details_views` slot is `None`.
        // - Resolved cards collect the rows into a hidden
        //   container behind a "▸ Details" disclosure. The card
        //   stays compact (URL + pills + outcome badge) so the
        //   "Recent" stack reads as a quick history rather than a
        //   wall of redundant detail; the structured rows are one
        //   click away when the user wants to audit a past
        //   decision.
        let effect_views: Vec<Retained<NSView>> = summary
            .parsed
            .as_ref()
            .map(|parsed| popover_effects::build_effect_views(parsed, mtm))
            .unwrap_or_default();
        let mut details_container: Option<Retained<NSView>> = None;
        if outcome.is_some() && !effect_views.is_empty() {
            // Resolved + has structured rows → wrap them in a
            // hidden container.
            let container = NSStackView::new(mtm);
            container.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            container.setSpacing(8.0);
            container.setDistribution(NSStackViewDistribution::Fill);
            for view in &effect_views {
                container.addArrangedSubview(view);
            }
            container.setHidden(true);

            let details_toggle = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("▸ Details"),
                    Some(&*(self as *const Self).cast::<AnyObject>()),
                    Some(sel!(toggleDetailsDisclosure:)),
                    mtm,
                )
            };
            details_toggle.setBordered(false);
            details_toggle.setButtonType(NSButtonType::PushOnPushOff);
            details_toggle.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            details_toggle.setTag(disclosure_idx as isize);
            card.addArrangedSubview(&details_toggle);
            card.addArrangedSubview(&container);

            details_container = Some(container.into_super());
        } else {
            // Pending card or no effect rows: render directly into
            // the card (no disclosure).
            for view in &effect_views {
                card.addArrangedSubview(view);
            }
        }
        // Always push a slot so `details_views` stays in lockstep
        // with `body_scrolls` and the global `disclosure_idx`. The
        // pending case stores `None` so the toggle action is a
        // no-op if it ever fires.
        self.ivars()
            .details_views
            .lock()
            .expect("details_views poisoned")
            .push(details_container);

        // "Show raw" disclosure — collapsed by default. Toggling
        // unhides the existing ANSI-attributed `body_scroll` so the
        // §8.5 layout stays one click away. We use a borderless
        // `PushOnPushOff` button with a chevron prefix in the
        // title rather than `NSBezelStyle::Disclosure` (which
        // *hides* the button's title and shows only a small `>`
        // triangle — the source of the "random `>` in the UI"
        // confusion in the first round). The action handler swaps
        // the title between `▸ Show raw` and `▾ Hide raw` so the
        // chevron orientation tracks the disclosure state.
        //
        // Both pending and resolved cards get a disclosure now;
        // the per-card `tag` is `disclosure_idx` (a global counter
        // managed by `refresh`) and indexes into the controller's
        // `body_scrolls` registry. This keeps "Recent" cards
        // visually consistent with pending ones — they previously
        // fell back to an always-on text view that dominated the
        // card.
        let raw_toggle = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("▸ Show raw"),
                Some(&*(self as *const Self).cast::<AnyObject>()),
                Some(sel!(toggleRawDisclosure:)),
                mtm,
            )
        };
        raw_toggle.setBordered(false);
        raw_toggle.setButtonType(NSButtonType::PushOnPushOff);
        raw_toggle.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        raw_toggle.setTag(disclosure_idx as isize);
        card.addArrangedSubview(&raw_toggle);

        // Hide the body by default. Register it in the
        // controller's per-card registry so the toggle action
        // can flip it. The `setHidden` call must come *before*
        // the height constraint below so AppKit doesn't try to
        // satisfy a nonzero size for a hidden view in the same
        // layout pass.
        body_scroll.setHidden(true);
        self.ivars()
            .body_scrolls
            .lock()
            .expect("body_scrolls poisoned")
            .push(body_scroll.clone());

        card.addArrangedSubview(&body_scroll);

        // Pending cards get Approve/Reject buttons. Resolved cards
        // already carry an outcome badge in the header; no further
        // action is possible so we omit the button row entirely.
        if outcome.is_none() {
            // Approve / Reject row. macOS HIG puts the destructive
            // (Reject) on the *left* and the default / accept
            // (Approve) on the *right*; we wire that up here.
            // Approve also gets `Return` as its key equivalent so
            // the user can tab into the popover and press Enter to
            // allow.
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
            // `\r` = Return. Promotes Approve to the system default
            // button so it picks up the accent tint and accepts Enter.
            approve.setKeyEquivalent(ns_string!("\r"));
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
            // Tints the bezel red on macOS 11+ and trips AppKit's
            // accidental-press guard. Older systems silently fall
            // back to a regular bezel.
            reject.setHasDestructiveAction(true);

            let buttons = NSStackView::new(mtm);
            buttons.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
            buttons.setSpacing(8.0);
            buttons.setDistribution(NSStackViewDistribution::Fill);
            buttons.addArrangedSubview(&reject);
            buttons.addArrangedSubview(&approve);
            card.addArrangedSubview(&buttons);
        }

        // Pin the body's height. `body_scroll` carries a `setFrame`
        // size, but once it's an arranged subview of an autolayout
        // `NSStackView` the frame is replaced by intrinsic-content
        // sizing — which for a freshly-built `scrollableTextView` is
        // basically zero. Without this constraint the §8.5 detail
        // collapses to a thin strip even when the outer cards stack
        // is correctly sized.
        //
        // The constant is the measured content height (clamped to
        // `RAW_BODY_MIN_HEIGHT..=RAW_BODY_MAX_HEIGHT`) rather than a
        // fixed `DETAIL_HEIGHT`. With `render_detail` reduced to
        // just the shell-quoted argv, most cards now show 1–2 lines
        // of monospaced text instead of a tall mostly-empty box;
        // pathological commands still get a scroller via
        // `setHasVerticalScroller(true)` once they exceed the cap.
        let body_height = content_height.clamp(RAW_BODY_MIN_HEIGHT, RAW_BODY_MAX_HEIGHT);
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[body_scroll
            .heightAnchor()
            .constraintEqualToConstant(body_height)]));

        // Dry-run cards used to be wrapped in a yellow `NSBox`, but
        // that wrapper made the cards visually inconsistent
        // (different sizing / inset only on dry-run cards) and
        // produced the "sometimes a yellow box around stuff but not
        // always" question in real usage. The cue now lives as a
        // small yellow pill at the trailing end of the header row
        // (added in the URL row construction above) so all cards
        // share the same chrome and the dry-run indicator is just
        // another tinted-pill atom in the existing visual
        // vocabulary.
        card.into_super()
    }
}

/// Measure the natural laid-out height of `tv` when wrapped to
/// `width` points wide.
///
/// Used by `build_card` to size the "Show raw" body to its content
/// (typically one or two lines of monospaced text now that
/// `render_detail` returns just the shell-quoted argv) instead of a
/// fixed `DETAIL_HEIGHT` that left every card with a tall mostly-
/// empty box.
///
/// We push `width` into the layout's `NSTextContainer` first so the
/// layout manager wraps to the same column we'll display at, then
/// force layout (`ensureLayoutForTextContainer:`) before reading
/// `usedRectForTextContainer:`. The text container's vertical inset
/// (`textContainerInset.height`) is added on top × 2 so the bottom
/// padding matches the top — `usedRect` reports just the glyph
/// rectangle.
///
/// Returns `RAW_BODY_MIN_HEIGHT` when AppKit's layout objects are
/// unexpectedly absent (shouldn't happen for a `scrollableTextView`
/// but we'd rather show a small box than crash here). The caller
/// clamps the returned value to `RAW_BODY_MIN/MAX_HEIGHT`.
fn measure_raw_body_height(tv: &NSTextView, width: f64) -> f64 {
    let container = match unsafe { tv.textContainer() } {
        Some(c) => c,
        None => return RAW_BODY_MIN_HEIGHT,
    };
    let layout = match unsafe { tv.layoutManager() } {
        Some(l) => l,
        None => return RAW_BODY_MIN_HEIGHT,
    };
    container.setContainerSize(NSSize::new(width, f64::MAX));
    layout.ensureLayoutForTextContainer(&container);
    let used = layout.usedRectForTextContainer(&container);
    used.size.height + tv.textContainerInset().height * 2.0
}

/// Find the first `HttpRequest` effect in `summary.parsed`, paired
/// with its index inside `parsed.effects` so the caller can index
/// into `summary.host_known`.
///
/// Returns `None` when `parsed` is absent (legacy / mock callers
/// that pre-date the `parsed` field) or when there is no
/// HttpRequest effect (today: ProcessSpawn-only requests are
/// theoretical; tomorrow: a future scp parser would land here).
/// In both cases the caller falls back to the plain
/// `verb target` label.
fn first_http_request(summary: &PromptSummary) -> Option<(&vetter_core::HttpRequest, usize)> {
    let parsed = summary.parsed.as_ref()?;
    parsed.effects.iter().enumerate().find_map(|(i, eff)| {
        if let vetter_core::Effect::HttpRequest(req) = eff {
            Some((req, i))
        } else {
            None
        }
    })
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
