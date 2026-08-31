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

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2::DefinedClass;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBezelStyle, NSBox,
    NSBoxType, NSButton, NSButtonType, NSColor, NSControlStateValueOff, NSControlStateValueOn,
    NSFont, NSFontWeightSemibold, NSImage, NSImageScaling, NSImageView, NSLayoutAttribute,
    NSLayoutConstraint, NSPasteboard, NSPasteboardTypeString, NSPopover, NSPopoverBehavior,
    NSPopoverDelegate, NSScrollView, NSStackView, NSStackViewDistribution, NSStatusBarButton,
    NSTextField, NSTextView, NSUserInterfaceLayoutOrientation, NSView, NSViewController,
    NSWorkspace,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSEdgeInsets, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSRectEdge, NSSize, NSString,
};

use super::{
    popover_attr, popover_effects, popover_picker, popover_pills, popover_url, AppDelegate,
};
use crate::pending::{PendingQueue, PromptSummary, ResolvedEntry};
use crate::Context;

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
    pub fn new(mtm: MainThreadMarker, ctx: Arc<Context>, delegate: &AppDelegate) -> Self {
        let queue = Arc::clone(&ctx.pending);
        // Build the cards stack first; the controller borrows a
        // strong ref to it so refresh can rebuild children.
        let cards = NSStackView::new(mtm);
        cards.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        cards.setSpacing(CARD_SPACING);
        cards.setDistribution(NSStackViewDistribution::Fill);
        // Pin the perpendicular alignment to `Leading` so every
        // card's left edge lines up with every other card's,
        // independent of intrinsic content width. The default
        // (`CenterX`) was centring narrower cards in the cards
        // stack, which made the resolved-card status icons land at
        // varying x positions across cards — defeating the whole
        // point of "scan the left edge to clock outcomes". Each
        // card's width is also pinned in `refresh` so right edges
        // align too and the pills row / dry-run pill have a
        // consistent right-edge gutter.
        cards.setAlignment(NSLayoutAttribute::Leading);
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

        // "Start at login" checkbox — anchored to the left edge of
        // the same footer strip. Tapping it routes through
        // `toggleAutostart:` on the AppDelegate, which persists the
        // preference + calls `SMAppService.{register,unregister}`
        // and rolls back the checkbox state on FFI failure (so the
        // UI never claims a state the OS rejected).
        //
        // Initial state is set to Off here; the real state is
        // refreshed on every `popoverWillShow:` via
        // `refresh_autostart_checkbox`.
        let autostart_checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Start at login"),
                Some(delegate_obj),
                Some(sel!(toggleAutostart:)),
                mtm,
            )
        };
        autostart_checkbox.setState(NSControlStateValueOff);
        // Tooltip echoes the System Settings escape hatch so users
        // know they can also flip this from outside Vetter.
        autostart_checkbox.setToolTip(Some(ns_string!(
            "Register Vetter.app as a Login Item via SMAppService. \
             You can also manage this in System Settings → General → Login Items."
        )));
        // Frame width 160pt covers the localized title plus the
        // checkbox glyph; height matches the Quit button so both
        // controls share the footer baseline.
        autostart_checkbox.setFrame(NSRect::new(
            NSPoint::new(12.0, 6.0),
            NSSize::new(160.0, 28.0),
        ));
        container.addSubview(&autostart_checkbox);

        // "Play sound on new request" checkbox — same footer strip,
        // immediately to the right of "Start at login". Routes
        // through `toggleNotificationSound:` on the AppDelegate,
        // which persists `Settings::notification_sound` (no OS API
        // to converge, unlike autostart's `SMAppService` call).
        //
        // Initial state is set to On here (matching
        // `Settings::default`); the real state is refreshed on
        // every `popoverWillShow:` via
        // `refresh_notification_sound_checkbox`.
        let notification_sound_checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Play sound on new request"),
                Some(delegate_obj),
                Some(sel!(toggleNotificationSound:)),
                mtm,
            )
        };
        notification_sound_checkbox.setState(NSControlStateValueOn);
        notification_sound_checkbox.setToolTip(Some(ns_string!(
            "Play the system default notification sound alongside \
             each approval banner. Respects Focus / Do Not Disturb \
             and your system notification-sound settings."
        )));
        // Frame width 230pt covers the longer title; sits between
        // the autostart checkbox (ends at x=172) and the Quit
        // button (starts at x=438).
        notification_sound_checkbox.setFrame(NSRect::new(
            NSPoint::new(184.0, 6.0),
            NSSize::new(230.0, 28.0),
        ));
        container.addSubview(&notification_sound_checkbox);

        let vc = NSViewController::new(mtm);
        vc.setView(&container);

        // Construct the controller before creating the popover so
        // we can install it as the popover's delegate (for the
        // popoverWillShow: refresh hook).
        let controller = PopoverController::new(mtm, ctx, queue, cards);
        controller
            .ivars()
            .autostart_checkbox
            .set(autostart_checkbox)
            .ok();
        controller
            .ivars()
            .notification_sound_checkbox
            .set(notification_sound_checkbox)
            .ok();

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

    /// Re-sync the autostart checkbox with the live OS state.
    /// Called from the AppDelegate's `toggleAutostart:` selector
    /// after a failed FFI call, so the visible state never diverges
    /// from what `[SMAppService.mainApp status]` returns.
    pub fn refresh_autostart_checkbox(&self) {
        self.controller.refresh_autostart_checkbox();
    }

    /// Force the checkbox into a specific boolean state. Used by
    /// the toggle selector when the user's intent and the OS reply
    /// disagree (e.g. user ticked On but `register` returned a
    /// `RequiresApproval`-style error before the next OS poll).
    pub fn set_autostart_checkbox_state(&self, enabled: bool) {
        self.controller.set_autostart_checkbox_state(enabled);
    }

    /// Re-sync the "Play sound on new request" checkbox with
    /// `~/.vet/settings.yaml`. Called on every `popoverWillShow:`
    /// and as the rollback path when persisting a toggle fails.
    pub fn refresh_notification_sound_checkbox(&self) {
        self.controller.refresh_notification_sound_checkbox();
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
    /// Full daemon context. Used by the Phase-5 picker actions
    /// (`allowlistClicked:` / `trustHostClicked:`) so the picker
    /// flow can call into [`crate::suggestions::*`] without going
    /// through the admin socket. The pending queue is reachable
    /// through `ctx.pending`; we cache it separately under `queue`
    /// to keep the existing `resolve_for_tag` call site cheap.
    ctx: std::sync::OnceLock<Arc<Context>>,
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
    /// Request ids keyed by the same global `disclosure_idx`
    /// counter `body_scrolls` / `details_views` use. Powers the
    /// Phase-5 `Allowlist…` / `Trust host…` buttons, which can
    /// appear on both pending and Allow-resolved cards (so the
    /// per-section `card_ids` mapping isn't enough).
    picker_ids: Mutex<Vec<String>>,
    /// Plain-text (no ANSI) version of each card's raw body,
    /// indexed by `disclosure_idx`. Populated in lockstep with
    /// `body_scrolls` during `build_card` so the per-card copy
    /// button can write the exact same bytes to the pasteboard
    /// that the user sees rendered in "Show raw". Kept separate
    /// from the attributed string inside `body_scrolls` because
    /// `NSTextView` selection + Cmd-C doesn't reliably escape the
    /// popover's transient first-responder state, which was the
    /// root cause of the "can't copy the raw contents" bug.
    raw_texts: Mutex<Vec<String>>,
    /// Per-button file paths registered by the per-effect "Open"
    /// buttons (Phase 5.1: inspect file inputs). Indexed by the
    /// button's `tag` — a flat counter that climbs across every
    /// card in the current refresh, independent of `disclosure_idx`
    /// (a single card can hold multiple file rows; FileRead and
    /// the matching `Body::FromFile` for `-d @file` each register
    /// their own button). Cleared in `refresh()` alongside the
    /// other per-card registries; the `openFileClicked:` selector
    /// uses `[sender tag]` to look the path back up.
    file_paths: Mutex<Vec<PathBuf>>,
    /// Per-card "See approval reason" disclosure body, indexed by
    /// `disclosure_idx`. `Some(view)` only on auto-allow Recent
    /// cards (those with a populated `rule_id` / `rule_scope`);
    /// every other slot stays `None` so a stray firing of the
    /// disclosure selector drops into a no-op. Powers Phase-5.1's
    /// rule-attribution surface.
    approval_reasons: Mutex<Vec<Option<Retained<NSView>>>>,
    /// Per-card revoke-rule targets, indexed by the same
    /// `disclosure_idx` slot `approval_reasons` uses. `Some((id,
    /// scope))` mirrors the auto-allow attribution stamped onto the
    /// resolved entry; the "Revoke rule" button reads this slot
    /// when clicked and hands the pair to
    /// [`super::popover_picker::confirm_and_revoke_rule`].
    revoke_targets: Mutex<Vec<Option<(String, vetter_core::matcher::Scope)>>>,
    /// "Start at login" checkbox in the popover footer. Refreshed
    /// from `[SMAppService.mainApp status]` on every
    /// `popoverWillShow:` so the UI reflects whatever the user may
    /// have flipped via System Settings → Login Items since the
    /// popover was last opened.
    autostart_checkbox: std::sync::OnceLock<Retained<NSButton>>,
    /// "Play sound on new request" checkbox in the popover footer.
    /// Refreshed from `~/.vet/settings.yaml`'s `notification_sound`
    /// field on every `popoverWillShow:`, mirroring
    /// `autostart_checkbox`'s re-sync so an out-of-band edit to the
    /// settings file is picked up next time the popover opens.
    notification_sound_checkbox: std::sync::OnceLock<Retained<NSButton>>,
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
            // Re-sync the checkbox with the OS-level Login Item
            // state. The user may have flipped this in System
            // Settings → Login Items between popover opens; we
            // never want the checkbox to advertise a state the OS
            // contradicts.
            self.refresh_autostart_checkbox();
            // Re-sync the sound checkbox with `settings.yaml` in
            // case it changed out-of-band since the popover was
            // last opened.
            self.refresh_notification_sound_checkbox();
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

        /// Per-card "Copy raw" selector. Looks up the plain-text
        /// raw body parked at `raw_texts[sender.tag]` and writes it
        /// to the general pasteboard via
        /// `NSPasteboardTypeString`. The button is a small SF Symbol
        /// glyph next to the "Show raw" disclosure toggle; using a
        /// dedicated button (rather than relying on the `NSTextView`
        /// selection + Cmd-C path) sidesteps the popover's flaky
        /// first-responder propagation, which made the documented
        /// "select text → Cmd-C" flow feel broken.
        #[unsafe(method(copyRawClicked:))]
        fn copy_raw_clicked(&self, sender: Option<&NSButton>) {
            self.copy_raw_for_tag(sender);
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

        /// "Allowlist…" picker selector. `sender.tag` indexes into
        /// `card_ids` from the most recent refresh; the matching id
        /// drives [`crate::suggestions::suggestions_for`] and the
        /// returned [`vetter_core::suggest::RuleSuggestion`] list
        /// powers the picker sheet.
        #[unsafe(method(allowlistClicked:))]
        fn allowlist_clicked(&self, sender: Option<&NSButton>) {
            self.open_allowlist_picker(sender);
        }

        /// "Trust host…" picker selector. Symmetric with
        /// `allowlistClicked:` but feeds
        /// [`vetter_core::suggest::HostSuggestion`] into the
        /// host picker. Hidden by `build_card` when the matching
        /// card has no `UnknownHost` signal, so a stray firing
        /// (out-of-range tag, picker offered with empty
        /// suggestions) drops into a no-op.
        #[unsafe(method(trustHostClicked:))]
        fn trust_host_clicked(&self, sender: Option<&NSButton>) {
            self.open_host_picker(sender);
        }

        /// Per-effect "Open file" selector (Phase 5.1). Looks up the
        /// path parked at `file_paths[sender.tag]` and asks
        /// `NSWorkspace` to open it via the user's default app — the
        /// approver wants to inspect the bytes a `-d @file` /
        /// `-T file` is about to upload before clicking Approve.
        /// Out-of-range tags / vanished paths drop silently; the
        /// button should not have been built in the first place but
        /// a stale popover snapshot can race with a delete on disk.
        #[unsafe(method(openFileClicked:))]
        fn open_file_clicked(&self, sender: Option<&NSButton>) {
            self.open_file_for_tag(sender);
        }

        /// "See approval reason" disclosure selector (Phase 5.1).
        /// Toggles visibility of the `approval_reasons[sender.tag]`
        /// view. Only auto-allow Recent cards register a slot here;
        /// pending cards and human-resolved Recent cards leave the
        /// slot `None` so a stray firing drops into a no-op.
        #[unsafe(method(toggleApprovalReasonDisclosure:))]
        fn toggle_approval_reason_disclosure(&self, sender: Option<&NSButton>) {
            self.toggle_approval_reason_for_tag(sender);
        }

        /// "Revoke rule" button selector (Phase 5.1). Reads the
        /// `(rule_id, scope)` slot at `revoke_targets[sender.tag]`
        /// and hands it to
        /// [`super::popover_picker::confirm_and_revoke_rule`] for
        /// the NSAlert confirm + async write flow. Out-of-range or
        /// `None` slots drop silently.
        #[unsafe(method(revokeRuleClicked:))]
        fn revoke_rule_clicked(&self, sender: Option<&NSButton>) {
            self.invoke_revoke_for_tag(sender);
        }
    }
);

impl PopoverController {
    fn new(
        mtm: MainThreadMarker,
        ctx: Arc<Context>,
        queue: Arc<PendingQueue>,
        cards: Retained<NSStackView>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PopoverControllerIvars::default());
        let this: Retained<Self> = unsafe { objc2::msg_send![super(this), init] };
        this.ivars().ctx.set(ctx).ok();
        this.ivars().queue.set(queue).ok();
        this.ivars().cards.set(cards).ok();
        this
    }

    fn ctx(&self) -> &Arc<Context> {
        self.ivars()
            .ctx
            .get()
            .expect("ctx is set in PopoverController::new")
    }

    /// Sync the "Start at login" checkbox with the current
    /// `[SMAppService.mainApp status]`. Called from
    /// `popoverWillShow:` so the UI never opens stale; also
    /// callable from the toggle selector after a failed FFI call
    /// so we roll the visible state back to whatever the OS
    /// actually has.
    pub(crate) fn refresh_autostart_checkbox(&self) {
        let Some(checkbox) = self.ivars().autostart_checkbox.get() else {
            return;
        };
        let status = crate::autostart::current();
        let new_state = if status.is_enabled() {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        };
        checkbox.setState(new_state);
        // Disable the checkbox entirely when the platform can't do
        // anything sensible with a click (non-bundle dev runs,
        // non-macOS ports). We surface the reason as the tooltip
        // so the user isn't left wondering why the control is
        // grey'd out.
        let interactive = !matches!(status, crate::autostart::AutostartStatus::Unsupported,);
        unsafe {
            // `setEnabled:` is part of NSControl; objc2-app-kit
            // exposes it on every `NSButton` subclass. Calling via
            // msg_send! avoids a feature gate on a method that's
            // present in every supported AppKit version.
            let _: () = objc2::msg_send![&**checkbox, setEnabled: interactive];
        }
    }

    /// Reflect a specific boolean back into the checkbox state.
    /// Used by the toggle selector when the FFI call rejects, so
    /// the UI reverts to whatever the OS still has.
    pub(crate) fn set_autostart_checkbox_state(&self, enabled: bool) {
        let Some(checkbox) = self.ivars().autostart_checkbox.get() else {
            return;
        };
        checkbox.setState(if enabled {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    /// Sync the "Play sound on new request" checkbox with the
    /// current `~/.vet/settings.yaml` value. Unlike
    /// `refresh_autostart_checkbox`, there's no OS API to query and
    /// no "unsupported platform" state to disable the control for —
    /// a missing/unreadable settings file just falls back to
    /// `Settings::default()` (sound on). Also doubles as the
    /// rollback path when `apply_notification_sound_change` fails
    /// to persist the write: re-reading the (unchanged) on-disk
    /// value snaps the checkbox back to what's actually saved.
    pub(crate) fn refresh_notification_sound_checkbox(&self) {
        let Some(checkbox) = self.ivars().notification_sound_checkbox.get() else {
            return;
        };
        let enabled = vetter_core::settings::load()
            .unwrap_or_default()
            .notification_sound;
        checkbox.setState(if enabled {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    fn id_for_tag(&self, sender: Option<&NSButton>) -> Option<String> {
        let button = sender?;
        let tag = button.tag();
        let g = self.ivars().picker_ids.lock().expect("picker_ids poisoned");
        g.get(tag as usize).cloned()
    }

    /// Body of `allowlistClicked:`. Looks up the request id for
    /// the clicked card, asks `suggestions::suggestions_for` for
    /// the rule candidates, then hands them to
    /// [`super::popover_picker::show_allowlist_picker`].
    /// Tag-out-of-range / unknown id / empty suggestion list all
    /// silently drop — the button should not have been visible in
    /// the first place, but a stale popover snapshot can race.
    fn open_allowlist_picker(&self, sender: Option<&NSButton>) {
        let Some(id) = self.id_for_tag(sender) else {
            return;
        };
        let Some((allowlist, _)) = crate::suggestions::suggestions_for(self.ctx(), &id) else {
            return;
        };
        if allowlist.is_empty() {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        popover_picker::show_allowlist_picker(mtm, Arc::clone(self.ctx()), allowlist);
    }

    /// Body of `trustHostClicked:`. Mirror image of
    /// `open_allowlist_picker` for the known-host suggestion list.
    fn open_host_picker(&self, sender: Option<&NSButton>) {
        let Some(id) = self.id_for_tag(sender) else {
            return;
        };
        let Some((_, host)) = crate::suggestions::suggestions_for(self.ctx(), &id) else {
            return;
        };
        if host.is_empty() {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        popover_picker::show_host_picker(mtm, Arc::clone(self.ctx()), host);
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

    /// Body of `copyRawClicked:`. Grabs the plain-text raw body
    /// associated with the clicked card and writes it to the
    /// general `NSPasteboard` as a single string entry, then swaps
    /// the button's SF Symbol image to a checkmark so the user
    /// sees a local confirmation that the copy landed. The
    /// confirmation glyph sticks until the popover closes and
    /// reopens (at which point `refresh` rebuilds every card from
    /// scratch, resetting the image to the clipboard glyph) —
    /// using a timer to restore it here would require shipping a
    /// `Retained<NSButton>` across a `Send` closure boundary, which
    /// objc2's main-thread-only retain types rightfully refuse.
    /// Tag-out-of-range drops silently: the button should not have
    /// been visible in the first place, but a stale popover
    /// snapshot can race.
    fn copy_raw_for_tag(&self, sender: Option<&NSButton>) {
        let Some(button) = sender else { return };
        let tag = button.tag();
        let text = {
            let g = self.ivars().raw_texts.lock().expect("raw_texts poisoned");
            g.get(tag as usize).cloned()
        };
        let Some(text) = text else {
            eprintln!("vetterd: copy-raw button tag {tag} out of range");
            return;
        };
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ok = pb.setString_forType(&NSString::from_str(&text), unsafe {
            NSPasteboardTypeString
        });
        if !ok {
            eprintln!("vetterd: NSPasteboard setString:forType: rejected copy-raw payload");
            return;
        }
        set_copy_button_confirming(button);
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

    /// Body of `toggleApprovalReasonDisclosure:`. Same per-tag
    /// disclosure pattern as `toggle_disclosure_for_tag`, but
    /// targets the `approval_reasons[sender.tag]` view (the body of
    /// the new "See approval reason" disclosure on auto-allow
    /// Recent cards). Returns silently for tags whose slot is
    /// `None` — pending cards and human-resolved Recent cards keep
    /// the slot empty.
    fn toggle_approval_reason_for_tag(&self, sender: Option<&NSButton>) {
        let Some(button) = sender else { return };
        let tag = button.tag();
        let off = NSControlStateValueOff;
        let hide = button.state() == off;
        let view = {
            let g = self
                .ivars()
                .approval_reasons
                .lock()
                .expect("approval_reasons poisoned");
            g.get(tag as usize).cloned().flatten()
        };
        if let Some(view) = view {
            view.setHidden(hide);
            let title = if hide {
                "▸ See approval reason"
            } else {
                "▾ Hide approval reason"
            };
            button.setTitle(&NSString::from_str(title));
        }
    }

    /// Body of `revokeRuleClicked:`. Reads the `(rule_id, scope)`
    /// pair stamped onto the auto-allow card at
    /// `revoke_targets[sender.tag]` and hands it to the picker's
    /// confirm+async helper. `None` slots / out-of-range tags drop
    /// silently — the button should not have been visible in the
    /// first place.
    fn invoke_revoke_for_tag(&self, sender: Option<&NSButton>) {
        let Some(button) = sender else { return };
        let tag = button.tag();
        let target = {
            let g = self
                .ivars()
                .revoke_targets
                .lock()
                .expect("revoke_targets poisoned");
            g.get(tag as usize).cloned().flatten()
        };
        let Some((rule_id, scope)) = target else {
            eprintln!("vetterd: revoke-rule button tag {tag} has no target");
            return;
        };
        let mtm =
            MainThreadMarker::new().expect("revokeRuleClicked: must be called on the main thread");
        let ctx = Arc::clone(self.ctx());
        super::popover_picker::confirm_and_revoke_rule(mtm, ctx, rule_id, scope);
    }

    /// Body of `openFileClicked:`. Looks up the path parked at
    /// `file_paths[sender.tag]` and asks `[NSWorkspace
    /// sharedWorkspace] openFile:` to dispatch it through Launch
    /// Services' content-type handler — Quick Look / TextEdit /
    /// VS Code etc., depending on the file's UTI.
    ///
    /// We deliberately use the deprecated `openFile:` rather than
    /// the modern `openURL:` here. `openURL:` for a `file://` URL
    /// routes through Launch Services' URL-handler chain, where
    /// any browser that has registered itself as a generic
    /// `file://` scheme handler (Firefox, Chrome with certain
    /// flags) intercepts the open and shows the file inside the
    /// browser instead of the user's actual editor. The
    /// content-type-keyed `openFile:` path bypasses scheme
    /// handlers entirely and matches what Finder's "Open" menu
    /// would do — which is the user's mental model when they
    /// click an Open button next to a file path.
    ///
    /// Tag-out-of-range / vanished file: drop silently (warn to
    /// stderr for the tag case so a refresh-vs-click race is
    /// debuggable). The button's existence implies `path.exists()`
    /// at the time `build_card` ran, but the file may have been
    /// removed since; `openFile:` returns `false` in that case and
    /// we surface a stderr line rather than pre-empting Finder's
    /// own error dialog.
    fn open_file_for_tag(&self, sender: Option<&NSButton>) {
        let Some(button) = sender else { return };
        let tag = button.tag();
        let path = {
            let g = self.ivars().file_paths.lock().expect("file_paths poisoned");
            g.get(tag as usize).cloned()
        };
        let Some(path) = path else {
            eprintln!("vetterd: open-file button tag {tag} out of range");
            return;
        };
        let path_str = path.to_string_lossy();
        let workspace = NSWorkspace::sharedWorkspace();
        // `openFile:` is marked deprecated in favour of `openURL:`,
        // but the replacement has the scheme-handler routing bug
        // documented above. The behaviour we want — "open this
        // file in the default app for its content type" — is what
        // `openFile:` has always done; AppKit still supports it on
        // every macOS version we target.
        #[allow(deprecated)]
        let opened = workspace.openFile(&NSString::from_str(&path_str));
        if !opened {
            eprintln!(
                "vetterd: NSWorkspace openFile: refused to open {}",
                path.display()
            );
        }
    }

    /// Build the "See approval reason" disclosure surface on an
    /// auto-allow Recent card (Phase 5.1).
    ///
    /// Adds two children to `card`:
    /// 1. A borderless `PushOnPushOff` toggle whose tag is
    ///    `disclosure_idx` so `toggle_approval_reason_for_tag` can
    ///    find the body when clicked.
    /// 2. A vertical stack hosting an attribution label
    ///    ("Auto-allowed by rule `<id>` in <scope> scope.") plus a
    ///    "Revoke rule" button. The stack starts hidden — the user
    ///    has to expand the disclosure to see it.
    ///
    /// Returns the body view so the caller can park it in the
    /// `approval_reasons` registry (the disclosure selector reads
    /// the slot back by tag).
    fn build_approval_reason_disclosure(
        &self,
        disclosure_idx: usize,
        rule_id: &str,
        scope: vetter_core::matcher::Scope,
        card: &NSStackView,
        mtm: MainThreadMarker,
    ) -> Retained<NSView> {
        // Toggle: same shape as the existing "Show raw" toggle so
        // the visual idiom stays consistent across disclosures on
        // a single card. Tag indexes into `approval_reasons`.
        let toggle = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("▸ See approval reason"),
                Some(&*(self as *const Self).cast::<AnyObject>()),
                Some(sel!(toggleApprovalReasonDisclosure:)),
                mtm,
            )
        };
        toggle.setBordered(false);
        toggle.setButtonType(NSButtonType::PushOnPushOff);
        toggle.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        toggle.setTag(disclosure_idx as isize);
        card.addArrangedSubview(&toggle);

        // Body: attribution sentence + Revoke button. Hidden
        // by default; the disclosure toggle flips `setHidden`
        // on the wrapping container.
        let body = NSStackView::new(mtm);
        body.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        body.setAlignment(NSLayoutAttribute::Leading);
        body.setSpacing(6.0);
        body.setDistribution(NSStackViewDistribution::Fill);

        let attribution_label = NSTextField::wrappingLabelWithString(
            &NSString::from_str(&format!(
                "Auto-allowed by rule `{rule_id}` in {} scope.",
                scope.as_str()
            )),
            mtm,
        );
        attribution_label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        attribution_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        attribution_label.setSelectable(true);
        body.addArrangedSubview(&attribution_label);

        let revoke = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Revoke rule"),
                Some(&*(self as *const Self).cast::<AnyObject>()),
                Some(sel!(revokeRuleClicked:)),
                mtm,
            )
        };
        // Bordered + small font so the destructive action looks
        // like a button (not a borderless inline link). The
        // disclosure title above it ("See approval reason") plus
        // the destructive `NSAlert` confirm flow gives enough
        // chrome — we deliberately don't try to colour the title
        // red, which would require an `NSAttributedString` dance
        // that doesn't compose well with system tint.
        revoke.setBezelStyle(BEZEL_ROUNDED);
        revoke.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        revoke.setTag(disclosure_idx as isize);
        body.addArrangedSubview(&revoke);

        body.setHidden(true);
        card.addArrangedSubview(&body);

        body.into_super()
    }

    /// Build the per-row "Open file" button used by the Phase 5.1
    /// inspect-file-input flow. Returns `None` when `path` doesn't
    /// exist on disk so writes-to-be-created and dangling
    /// references stay button-free; otherwise pushes `path` onto
    /// `file_paths`, builds a borderless SF-Symbol button, and
    /// stamps the new index onto `setTag` so `open_file_for_tag`
    /// can find the path back.
    ///
    /// `target_ptr` is the controller itself (shared across every
    /// row in a single refresh). The unsafe cast mirrors how the
    /// Approve/Reject buttons borrow the controller pointer.
    fn build_open_file_button(
        &self,
        path: &std::path::Path,
        target_ptr: *const AnyObject,
        mtm: MainThreadMarker,
    ) -> Option<Retained<NSButton>> {
        if !path.exists() {
            return None;
        }
        let tag = {
            let mut g = self.ivars().file_paths.lock().expect("file_paths poisoned");
            let tag = g.len();
            g.push(path.to_path_buf());
            tag
        };
        let btn = build_open_file_button(mtm);
        // SAFETY: target_ptr is the live `&PopoverController` we
        // were called from; it outlives the button because the
        // button is dropped when `refresh()` rebuilds the cards
        // stack — and `refresh()` only runs from the same
        // controller, on the same main thread.
        unsafe {
            btn.setTarget(Some(&*target_ptr));
            btn.setAction(Some(sel!(openFileClicked:)));
        }
        btn.setTag(tag as isize);
        Some(btn)
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
        self.ivars()
            .picker_ids
            .lock()
            .expect("picker_ids poisoned")
            .clear();
        self.ivars()
            .raw_texts
            .lock()
            .expect("raw_texts poisoned")
            .clear();
        self.ivars()
            .file_paths
            .lock()
            .expect("file_paths poisoned")
            .clear();
        self.ivars()
            .approval_reasons
            .lock()
            .expect("approval_reasons poisoned")
            .clear();
        self.ivars()
            .revoke_targets
            .lock()
            .expect("revoke_targets poisoned")
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
        if entries.is_empty() {
            // Pending block is empty but `resolved` isn't (the
            // both-empty case returned early above). Show a quiet
            // "no pending" placeholder where the pending cards
            // would normally be, so the popover doesn't open
            // straight onto the Recent section with no context —
            // the user otherwise has to read the "Recent" header
            // to figure out why their just-opened popover has no
            // action buttons. Same secondary-label styling as the
            // "Recent" header below for visual consistency.
            let no_pending = NSTextField::labelWithString(ns_string!("No pending approvals."), mtm);
            no_pending.setTextColor(Some(&NSColor::secondaryLabelColor()));
            no_pending.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            no_pending.setAlignment(objc2_app_kit::NSTextAlignment::Center);
            add_full_width_arranged(cards, &no_pending);
        }
        for (idx, (summary, rendered)) in entries.iter().enumerate() {
            // Thin horizontal rule between adjacent cards.
            if idx > 0 {
                let sep = NSBox::new(mtm);
                sep.setBoxType(NSBoxType::Separator);
                add_full_width_arranged(cards, &sep);
            }
            let card = self.build_card(mtm, idx, disclosure_idx, summary, rendered, None, None);
            disclosure_idx += 1;
            if focused_id.is_some_and(|f| f == summary.id) {
                focused_view = Some(card.clone());
            }
            add_full_width_arranged(cards, &card);
        }

        if !resolved.is_empty() {
            // "Recent" header sits between the pending block and the
            // resolved block. We always drop a separator before it
            // — when pending cards precede it the rule splits the
            // two sections, and when only the placeholder
            // ("No pending approvals.") is above we still want a
            // visual divide so the placeholder doesn't look like
            // it belongs to the Recent group.
            let sep = NSBox::new(mtm);
            sep.setBoxType(NSBoxType::Separator);
            add_full_width_arranged(cards, &sep);
            let section_label = NSTextField::labelWithString(ns_string!("Recent"), mtm);
            section_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            section_label.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
            add_full_width_arranged(cards, &section_label);

            for (idx, entry) in resolved.iter().enumerate() {
                let sep = NSBox::new(mtm);
                sep.setBoxType(NSBoxType::Separator);
                add_full_width_arranged(cards, &sep);
                // `idx` is unused for resolved cards' buttons (the
                // outcome is already final, so they have no
                // Approve/Reject row); we still pass it so
                // `build_card`'s signature stays uniform.
                // `disclosure_idx` keeps climbing so this card's
                // `Show raw` button lands on its own slot in
                // `body_scrolls`.
                // Auto-allow attribution drives the new "See
                // approval reason" disclosure on Allow-resolved
                // cards. We only forward it on `Allow` outcomes —
                // a Deny card with a rule_id (denylist hit) would
                // surface a Revoke button for a denylist rule we
                // deliberately don't edit from the popover today.
                let attribution = match (entry.decision, &entry.rule_id, &entry.rule_scope) {
                    (vetter_core::wire::WireDecision::Allow, Some(id), Some(scope)) => {
                        Some((id.as_str(), *scope))
                    }
                    _ => None,
                };
                let card = self.build_card(
                    mtm,
                    idx,
                    disclosure_idx,
                    &entry.summary,
                    &entry.rendered,
                    Some(&entry.decision),
                    attribution,
                );
                disclosure_idx += 1;
                add_full_width_arranged(cards, &card);
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
    ///
    /// `attribution` carries the matcher's `(rule_id, scope)` for
    /// auto-allow Recent cards. `Some(...)` triggers the new "See
    /// approval reason" disclosure + Revoke rule button (Phase 5.1);
    /// `None` for pending cards and human-resolved Recent cards
    /// (no rule was involved). Callers must align the slot they
    /// stamp into `approval_reasons` / `revoke_targets` with the
    /// per-card `disclosure_idx`.
    #[allow(clippy::too_many_arguments)]
    fn build_card(
        &self,
        mtm: MainThreadMarker,
        idx: usize,
        disclosure_idx: usize,
        summary: &PromptSummary,
        rendered: &str,
        outcome: Option<&vetter_core::wire::WireDecision>,
        attribution: Option<(&str, vetter_core::matcher::Scope)>,
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
        // Align children to the top of the header row so the
        // command label stays flush with the *first* line of the
        // URL view, even when the URL's path/query wraps onto a
        // second row inside `popover_url::build_url_row`. The
        // default `CenterY` alignment used to float "curl" halfway
        // down a tall url_view, which read as a layout bug.
        header_row.setAlignment(NSLayoutAttribute::Top);
        // Resolved cards lead with a coloured status glyph (green
        // check / red X) so the user can scan the left edge of the
        // Recent stack and clock every past outcome at a glance —
        // the older trailing "Allowed"/"Denied" text label put the
        // status at the right edge where it competed with the URL
        // for attention. Pending cards skip this icon (no decision
        // yet) and lean on the Approve/Reject buttons at the
        // bottom of the card to communicate state.
        if let Some(decision) = outcome {
            if let Some(icon) = build_outcome_icon(*decision, mtm) {
                header_row.addArrangedSubview(&icon);
            }
        }
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
        // Left-align every row inside the card. With the default
        // `CenterX` alignment, narrow rows (header_row when the URL
        // is short, pills row when only one chip is rendered)
        // floated towards the card's centre, which dragged the
        // resolved-card status icon away from the leading edge —
        // so two cards with different content widths showed icons
        // at different x positions even though the cards
        // themselves were now full-width. `Leading` keeps each
        // row's leading edge flush with the card's content
        // gutter.
        card.setAlignment(NSLayoutAttribute::Leading);
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

        // Per-effect native rows: headers (names only — values are
        // never shown in the popover; see `build_header_row` for
        // the rationale), body (typed per `Body` variant), auth,
        // file ops, process spawns. Falls back to nothing when
        // `parsed` is absent (legacy / mock callers); the "Show
        // raw" disclosure below still surfaces the §8.5 layout in
        // that case.
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
        // Phase 5.1: per-row "Open file" buttons on FileRead /
        // `Body::FromFile` rows. The factory closure pushes the
        // resolved path onto `file_paths`, returns the matching tag
        // wired to `openFileClicked:`, and silently bows out for
        // paths that don't exist on disk yet (so writes-to-be-
        // created and dangling references don't sprout a useless
        // button). The `target` pointer is the controller itself —
        // shared across every row in this refresh, mirrors how the
        // Approve/Reject buttons borrow `self` for their selectors.
        let target_ptr: *const AnyObject = (self as *const Self).cast::<AnyObject>();
        let file_button_factory = |path: &std::path::Path, mtm: MainThreadMarker| {
            self.build_open_file_button(path, target_ptr, mtm)
        };
        let effect_views = summary
            .parsed
            .as_ref()
            .map(|parsed| popover_effects::build_effect_views(parsed, mtm, &file_button_factory))
            .unwrap_or_else(|| popover_effects::EffectViews {
                file_inputs: Vec::new(),
                others: Vec::new(),
            });
        let mut details_container: Option<Retained<NSView>> = None;
        if outcome.is_some() {
            // Resolved card. File-input rows always render inline
            // so the per-row "Open file" button stays one click
            // away after the request lands in "Recent" — the user
            // often comes back to a recently approved card to
            // re-inspect what they uploaded. The other effect rows
            // (headers, non-file body, auth, file writes, process
            // spawns) sit behind a "▸ Details" disclosure so the
            // Recent stack stays compact when only the audit
            // header matters.
            for view in &effect_views.file_inputs {
                card.addArrangedSubview(view);
            }
            if !effect_views.others.is_empty() {
                let container = NSStackView::new(mtm);
                container.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                container.setSpacing(8.0);
                container.setDistribution(NSStackViewDistribution::Fill);
                for view in &effect_views.others {
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
            }
        } else {
            // Pending card. Render every per-effect row inline in
            // source order — the user is making a decision *now*
            // and wants to see headers / body / auth / file ops
            // without reaching for a disclosure. Painting
            // `file_inputs` first matches the resolved layout and
            // keeps the "what file are you uploading?" question at
            // the top of every card.
            for view in &effect_views.file_inputs {
                card.addArrangedSubview(view);
            }
            for view in &effect_views.others {
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

        // "See approval reason" disclosure (Phase 5.1) — only built
        // for auto-allow Recent cards (those with a populated
        // `attribution`). Pending cards have nothing to show here
        // (no rule fired yet); human-resolved cards have no rule to
        // attribute; deny-resolved cards aren't editable from the
        // popover today. We push a slot into both registries on
        // every card regardless, so the per-tag lookups stay
        // aligned with `disclosure_idx`.
        let approval_view: Option<Retained<NSView>> = attribution.map(|(rule_id, scope)| {
            self.build_approval_reason_disclosure(disclosure_idx, rule_id, scope, &card, mtm)
        });
        self.ivars()
            .approval_reasons
            .lock()
            .expect("approval_reasons poisoned")
            .push(approval_view);
        self.ivars()
            .revoke_targets
            .lock()
            .expect("revoke_targets poisoned")
            .push(attribution.map(|(id, scope)| (id.to_string(), scope)));

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

        // Per-card copy-to-clipboard button. Lives in a horizontal
        // stack alongside the "Show raw" toggle so the visual
        // cluster reads as "raw-body controls". The button is
        // always present (even when the body is collapsed) because
        // the common path is "glance at the URL row, copy the
        // command to paste into a shell" — forcing the user to
        // disclose the body first just adds a click.
        //
        // NSPopover's first-responder handling does not reliably
        // propagate Cmd-C to the `NSTextView` inside `body_scroll`
        // (the document view is selectable but the keyboard focus
        // chain drops the key event), so the dedicated button is
        // the supported copy path — not just a convenience.
        let copy_btn = build_copy_button(mtm);
        let target_ptr: *const AnyObject = (self as *const Self).cast::<AnyObject>();
        unsafe {
            copy_btn.setTarget(Some(&*target_ptr));
            copy_btn.setAction(Some(sel!(copyRawClicked:)));
        }
        copy_btn.setTag(disclosure_idx as isize);

        let raw_row = NSStackView::new(mtm);
        raw_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        raw_row.setSpacing(6.0);
        raw_row.setDistribution(NSStackViewDistribution::Fill);
        raw_row.addArrangedSubview(&raw_toggle);
        raw_row.addArrangedSubview(&copy_btn);
        // Flexible spacer so both controls cluster left; without it
        // the copy button drifts towards the middle of the card on
        // the wider popover layouts.
        let raw_row_spacer = NSView::new(mtm);
        raw_row.addArrangedSubview(&raw_row_spacer);
        card.addArrangedSubview(&raw_row);

        // Register the plain (no-ANSI) raw text under the same
        // global `disclosure_idx` slot the copy button's `tag`
        // encodes, so `copy_raw_for_tag` can find it.
        self.ivars()
            .raw_texts
            .lock()
            .expect("raw_texts poisoned")
            .push(popover_attr::strip_ansi(rendered));

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

        // Phase-5 picker buttons. Available on:
        // - every pending card (so the user can pre-emptively
        //   broaden the allowlist instead of one-shot Approving)
        // - every Allow / AllowOnce resolved card (the user often
        //   realises post-decision that they want to memoise the
        //   pattern; this row keeps the affordance one click away).
        // Deny-resolved cards intentionally hide both buttons —
        // surfacing "Allowlist…" right after the user denied a
        // request would be confusing at best.
        let allow_picker = matches!(
            outcome,
            None | Some(vetter_core::wire::WireDecision::Allow)
                | Some(vetter_core::wire::WireDecision::AllowOnce)
        );
        if allow_picker {
            // Register the request id under the same global
            // `disclosure_idx` slot we'll use for the buttons' tag.
            // Done unconditionally even when no buttons end up
            // visible (e.g. the engine returned zero suggestions)
            // so the registry stays in lockstep with the global
            // counter — the action handler short-circuits on empty
            // suggestion lists anyway.
            self.ivars()
                .picker_ids
                .lock()
                .expect("picker_ids poisoned")
                .push(summary.id.clone());

            let picker_row = NSStackView::new(mtm);
            picker_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
            picker_row.setSpacing(8.0);
            picker_row.setDistribution(NSStackViewDistribution::Fill);

            let allow_btn = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("Allowlist…"),
                    Some(&*(self as *const Self).cast::<AnyObject>()),
                    Some(sel!(allowlistClicked:)),
                    mtm,
                )
            };
            allow_btn.setBordered(false);
            allow_btn.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            allow_btn.setTag(disclosure_idx as isize);
            picker_row.addArrangedSubview(&allow_btn);

            // Trust host… is gated on the `UnknownHost` signal
            // being present — when the host is already trusted
            // (builtin or previously-added), there is nothing to
            // suggest. Mirrors the `host_suggestions(...)` engine
            // contract so the button visibility matches the
            // picker's emission rule.
            let has_unknown = summary
                .signals
                .iter()
                .any(|s| s.kind == vetter_core::SignalKind::UnknownHost);
            if has_unknown {
                let host_btn = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        ns_string!("Trust host…"),
                        Some(&*(self as *const Self).cast::<AnyObject>()),
                        Some(sel!(trustHostClicked:)),
                        mtm,
                    )
                };
                host_btn.setBordered(false);
                host_btn.setFont(Some(&NSFont::systemFontOfSize(11.0)));
                host_btn.setTag(disclosure_idx as isize);
                picker_row.addArrangedSubview(&host_btn);
            }

            // Trailing flexible spacer so the buttons cluster
            // left, leaving the right edge of the row clean for
            // future overflow controls (e.g. `…` menu).
            let spacer = NSView::new(mtm);
            picker_row.addArrangedSubview(&spacer);
            card.addArrangedSubview(&picker_row);
        } else {
            // Deny-resolved card: keep the registry slot so
            // disclosure_idx-based tags stay aligned. We push a
            // sentinel that the action handlers will never look up
            // (Deny cards don't render any picker buttons).
            self.ivars()
                .picker_ids
                .lock()
                .expect("picker_ids poisoned")
                .push(String::new());
        }

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

/// Add `view` as an arranged subview of the cards stack and pin its
/// width to the stack's width minus the horizontal margin on both
/// sides.
///
/// `NSStackView` with `Leading` alignment and no width constraint
/// lets each arranged subview stay at its intrinsic content width,
/// which made cards (and separators) look like ragged-right islands
/// on the dark popover background. Pinning width here forces every
/// card to span the full content area so the right edge stays as
/// straight as the left edge — and the per-card pills row / dry-
/// run pill have a consistent right-edge gutter to push against.
///
/// Same idiom is used for the `NSBox` separators and the "Recent"
/// section header so the dividing rules and label span the full
/// width too, instead of shrinking to their intrinsic size and
/// leaving an awkward leading-aligned stub.
fn add_full_width_arranged(cards: &NSStackView, view: &NSView) {
    cards.addArrangedSubview(view);
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[view
        .widthAnchor()
        .constraintEqualToAnchor_constant(&cards.widthAnchor(), -2.0 * CARDS_HORIZONTAL_MARGIN)]));
}

/// SF Symbol point size for the resolved-card status icon. Tuned to
/// roughly match the cap height of the bold 13pt monospaced
/// command label next to it so the icon and text baseline align.
const OUTCOME_ICON_SIZE: f64 = 16.0;

/// Build the leading status glyph for a resolved card: a green
/// `checkmark.circle.fill` for Allow / AllowOnce, a red
/// `xmark.circle.fill` for Deny.
///
/// Returns `None` when the running OS doesn't ship the requested SF
/// Symbol (older macOS, future symbol renames). The caller treats
/// the icon as decorative — its absence drops the card back to a
/// text-only header without breaking layout.
///
/// Tinting goes through `setContentTintColor:`. SF Symbol images
/// are templates, so the tint colour applies uniformly across the
/// glyph regardless of the system theme — under the popover's
/// pinned Dark Aqua appearance this gives a vivid green / red on
/// the dark surface, exactly the "scan the left edge" affordance
/// the resolved cards are tuned for.
fn build_outcome_icon(
    decision: vetter_core::wire::WireDecision,
    mtm: MainThreadMarker,
) -> Option<Retained<NSView>> {
    let (symbol, color) = match decision {
        vetter_core::wire::WireDecision::Allow | vetter_core::wire::WireDecision::AllowOnce => {
            ("checkmark.circle.fill", NSColor::systemGreenColor())
        }
        vetter_core::wire::WireDecision::Deny => ("xmark.circle.fill", NSColor::systemRedColor()),
    };
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&NSString::from_str(match decision {
            vetter_core::wire::WireDecision::Allow | vetter_core::wire::WireDecision::AllowOnce => {
                "Allowed"
            }
            vetter_core::wire::WireDecision::Deny => "Denied",
        })),
    )?;
    let view = NSImageView::imageViewWithImage(&image, mtm);
    view.setContentTintColor(Some(&color));
    view.setFrameSize(NSSize::new(OUTCOME_ICON_SIZE, OUTCOME_ICON_SIZE));
    // NSImageView → NSControl → NSView. Two `into_super` hops to
    // land on the storage type used by the rest of the header row.
    Some(view.into_super().into_super())
}

/// Build the per-card "Copy raw" button — a borderless SF Symbol
/// glyph next to the "Show raw" disclosure toggle. Tapping it
/// copies the plain-text raw body to the general pasteboard
/// (see [`PopoverController::copy_raw_for_tag`]). Wiring the
/// target/selector is the caller's job because the selector is
/// defined on `PopoverController` and we don't want to plumb a
/// `Retained<PopoverController>` through every card builder.
///
/// Falls back to a text-glyph title (`"⧉"`) when the SF Symbol
/// lookup fails — on macOS 11 `doc.on.clipboard` is present, but a
/// future system or a stripped SF Symbols install could still
/// return `None`; the accessibility path should keep working.
fn build_copy_button(mtm: MainThreadMarker) -> Retained<NSButton> {
    let btn = NSButton::new(mtm);
    btn.setBordered(false);
    btn.setBezelStyle(BEZEL_ROUNDED);
    btn.setButtonType(NSButtonType::MomentaryChange);
    btn.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
    btn.setToolTip(Some(ns_string!("Copy raw command to clipboard")));
    set_copy_button_idle(&btn);
    // Constrain the button to a compact square so it sits flush
    // with the 11pt "Show raw" text and doesn't hijack the
    // horizontal stack's intrinsic-content allocation.
    let side = 18.0;
    btn.setFrameSize(NSSize::new(side, side));
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        btn.widthAnchor().constraintEqualToConstant(side),
        btn.heightAnchor().constraintEqualToConstant(side),
    ]));
    btn
}

/// Build the per-effect "Open file" button (Phase 5.1) — a
/// borderless SF Symbol glyph that sits between the file path and
/// the trailing spacer in a `popover_effects::file_glyph_row_with_open`.
/// Tapping it asks the user's default app to open the file via
/// [`PopoverController::open_file_for_tag`].
///
/// Wiring the target / selector / tag is the caller's job (see
/// [`PopoverController::build_open_file_button`]) — same split as
/// [`build_copy_button`] so we don't drag a `Retained<PopoverController>`
/// down through every row helper.
///
/// Falls back to a text-glyph title when the SF Symbol lookup
/// fails so older macOS / stripped SF Symbols installs still get a
/// clickable affordance.
fn build_open_file_button(mtm: MainThreadMarker) -> Retained<NSButton> {
    let btn = NSButton::new(mtm);
    btn.setBordered(false);
    btn.setBezelStyle(BEZEL_ROUNDED);
    btn.setButtonType(NSButtonType::MomentaryChange);
    btn.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
    btn.setToolTip(Some(ns_string!("Open file in default app")));
    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        ns_string!("arrow.up.right.square"),
        Some(ns_string!("Open file in default app")),
    ) {
        btn.setImage(Some(&img));
        btn.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        btn.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        btn.setTitle(ns_string!(""));
    } else {
        btn.setTitle(ns_string!("↗"));
    }
    let side = 18.0;
    btn.setFrameSize(NSSize::new(side, side));
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        btn.widthAnchor().constraintEqualToConstant(side),
        btn.heightAnchor().constraintEqualToConstant(side),
    ]));
    btn
}

/// Stamp the idle (ready-to-copy) SF Symbol onto `btn`. Called from
/// `build_copy_button` at card build time; `refresh` rebuilds every
/// card on popover open so this naturally doubles as the reset
/// after a prior "Copied" confirmation.
fn set_copy_button_idle(btn: &NSButton) {
    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        ns_string!("doc.on.clipboard"),
        Some(ns_string!("Copy raw command to clipboard")),
    ) {
        btn.setImage(Some(&img));
        btn.setImageScaling(NSImageScaling::ScaleProportionallyDown);
        btn.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        btn.setTitle(ns_string!(""));
    } else {
        // SF Symbol missing — fall back to a bracketed-copy glyph.
        // The layout width constraint keeps the button the same
        // size regardless of which render path we hit.
        btn.setTitle(ns_string!("⧉"));
    }
}

/// Flip `btn` to a "just copied" confirmation: green checkmark
/// glyph + matching tint. No automatic reversion — the next
/// `refresh` (triggered by popoverWillShow or any queue change)
/// rebuilds the card and hence the button, so the idle glyph
/// comes back naturally. See [`PopoverController::copy_raw_for_tag`]
/// for why we don't dispatch a restore timer.
fn set_copy_button_confirming(btn: &NSButton) {
    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        ns_string!("checkmark"),
        Some(ns_string!("Copied")),
    ) {
        btn.setImage(Some(&img));
        btn.setContentTintColor(Some(&NSColor::systemGreenColor()));
        btn.setTitle(ns_string!(""));
    } else {
        btn.setTitle(ns_string!("✓"));
    }
    btn.setToolTip(Some(ns_string!("Copied to clipboard")));
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
