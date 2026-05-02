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
    NSBezelStyle, NSBox, NSBoxType, NSButton, NSColor, NSFont, NSFontWeightSemibold,
    NSLayoutConstraint, NSPopover, NSPopoverBehavior, NSPopoverDelegate, NSScrollView, NSStackView,
    NSStackViewDistribution, NSStatusBarButton, NSTextField, NSTextView, NSTitlePosition,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSEdgeInsets, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSRectEdge, NSSize, NSString,
};

use super::{popover_attr, AppDelegate};
use crate::pending::{PendingQueue, PromptSummary, ResolvedEntry};

/// Outer popover dimensions. Width is fixed; height grows with
/// content up to this cap, then the inner scroll view scrolls.
const POPOVER_WIDTH: f64 = 480.0;
const POPOVER_HEIGHT: f64 = 500.0;
/// Outer gap between adjacent cards in the cards stack. Bumped from
/// 12 → 16 once we started inserting `NSBoxType::Separator` lines
/// between cards: the rule line wants a bit more breathing room
/// either side or it visually crowds the buttons.
const CARD_SPACING: f64 = 16.0;
const CARD_PADDING: f64 = 12.0;
/// Inset around each card's content (header / body / buttons). Pulls
/// the body away from any wrapping `NSBox` border (dry-run case) and
/// gives non-dry-run cards consistent breathing room without needing
/// a wrapper view.
const CARD_INSET: f64 = 12.0;
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

        if entries.is_empty() && resolved.is_empty() {
            let empty = NSTextField::labelWithString(ns_string!("No pending requests."), mtm);
            empty.setAlignment(objc2_app_kit::NSTextAlignment::Center);
            cards.addArrangedSubview(&empty);
            return;
        }

        let mut focused_view: Option<Retained<NSView>> = None;
        for (idx, (summary, rendered)) in entries.iter().enumerate() {
            // Thin horizontal rule between adjacent cards.
            if idx > 0 {
                let sep = NSBox::new(mtm);
                sep.setBoxType(NSBoxType::Separator);
                cards.addArrangedSubview(&sep);
            }
            let card = self.build_card(mtm, idx, summary, rendered, None);
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
                // `idx` is unused for resolved cards (no buttons → no
                // tag) but `build_card` still needs it for the
                // pending-card branch.
                let card = self.build_card(
                    mtm,
                    idx,
                    &entry.summary,
                    &entry.rendered,
                    Some(&entry.decision),
                );
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
        summary: &PromptSummary,
        rendered: &str,
        outcome: Option<&vetter_core::wire::WireDecision>,
    ) -> Retained<NSView> {
        // Header: "<command> <verb> <target>". The inline "dry run"
        // pill that used to live here is now absorbed by the outer
        // `NSBox` wrapper (see end of this function); the box's
        // yellow-bordered title is a louder cue than a grey
        // secondary-label word in the same row.
        let header_text = if summary.primary_verb.is_empty() {
            format!("{}  {}", summary.command, summary.primary_target)
        } else {
            format!(
                "{}  {} {}",
                summary.command, summary.primary_verb, summary.primary_target
            )
        };
        let header = NSTextField::labelWithString(&NSString::from_str(&header_text), mtm);
        // Use `monospacedSystemFontOfSize:weight:` semibold so URLs /
        // paths / methods in the header line up vertically with the
        // monospaced body text below. `NSFontWeightSemibold` is a
        // C `extern static` (unsafe to read) but holds a fixed
        // CGFloat — caching is fine.
        let semibold = unsafe { NSFontWeightSemibold };
        header.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            13.0, semibold,
        )));
        // Resolved cards use a dimmer header colour to signal that
        // they are informational (no action required).
        if outcome.is_some() {
            header.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }

        let header_row = NSStackView::new(mtm);
        header_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        header_row.setSpacing(8.0);
        header_row.setDistribution(NSStackViewDistribution::Fill);
        header_row.addArrangedSubview(&header);

        // Risk-signal chips — one per `Warn`/`Danger`-tier signal.
        // Info-tier kinds (none today) intentionally render only in
        // the body's `Risk signals:` line so chip space is reserved
        // for "actually look at this" cues. We dedupe by `SignalKind`
        // because the analyzer emits one entry per effect (e.g. a
        // POST + AuthHeader on the same request would otherwise paint
        // two `auth-header` chips).
        let mut seen: Vec<vetter_core::SignalKind> = Vec::new();
        for kind in &summary.signals {
            if seen.contains(kind) {
                continue;
            }
            seen.push(*kind);
            let severity = kind.ui_severity();
            let color = match severity {
                vetter_core::BadgeSeverity::Danger => NSColor::systemRedColor(),
                vetter_core::BadgeSeverity::Warn => NSColor::systemOrangeColor(),
                // Info-tier: skip entirely; the body line carries it.
                vetter_core::BadgeSeverity::Info => continue,
            };
            let label = vetter_core::signal_kind_label(*kind);
            let chip = NSTextField::labelWithString(&NSString::from_str(label), mtm);
            chip.setFont(Some(&NSFont::boldSystemFontOfSize(10.0)));
            chip.setTextColor(Some(&color));
            header_row.addArrangedSubview(&chip);
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
        body_scroll.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(POPOVER_WIDTH - CARD_PADDING * 2.0, DETAIL_HEIGHT),
        ));
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
            tv.setBackgroundColor(&NSColor::textBackgroundColor());
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
        NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[body_scroll
            .heightAnchor()
            .constraintEqualToConstant(DETAIL_HEIGHT)]));

        if summary.force_prompt {
            // Dry-run cards get a custom `NSBox` with a yellow
            // border and a "dry run" title. This replaces the
            // inline secondary-label pill that used to live in
            // `header_row` — visually louder, and clearly scopes
            // *which* card is the dry-run when several requests
            // are queued up.
            let box_ = NSBox::new(mtm);
            box_.setBoxType(NSBoxType::Custom);
            box_.setBorderColor(&NSColor::systemYellowColor());
            box_.setBorderWidth(1.5);
            box_.setCornerRadius(6.0);
            box_.setTitlePosition(NSTitlePosition::AtTop);
            box_.setTitle(ns_string!("dry run"));
            box_.setContentView(Some(&card));
            box_.into_super()
        } else {
            // Non-dry-run cards stay bare — visual contrast with
            // dry-run cards reads at a glance.
            card.into_super()
        }
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
