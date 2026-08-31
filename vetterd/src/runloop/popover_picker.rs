//! Picker sheets surfaced from the popover's per-card "Allowlist…"
//! / "Trust host…" actions.
//!
//! Each picker is an `NSAlert` with a vertical `NSStackView`
//! `accessoryView` of radio buttons (one per
//! [`vetter_core::suggest::SuggestionTier`] /
//! [`vetter_core::suggest::HostTier`]). The label below each radio
//! shows the YAML preview of the rule / host that would be
//! persisted; the user picks one and clicks **Add to user
//! allowlist** / **Trust this host**, which dispatches the persist
//! call onto the global concurrent queue (so the file IO + YAML
//! reload doesn't stall the main thread). Success / error surfaces
//! through a follow-up `NSAlert` on the main queue.
//!
//! All AppKit calls live behind `#[cfg(target_os = "macos")]` —
//! the popover module already gates on the same target so this
//! file is only compiled into the daemon binary, never the
//! integration-test wire harness.

use std::sync::{Arc, Mutex};

use dispatch2::{DispatchQoS, DispatchQueue, GlobalQueueIdentifier};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSAppearance, NSAppearanceCustomization,
    NSAppearanceNameDarkAqua, NSButton, NSButtonType, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSFont, NSLayoutAttribute, NSLayoutConstraint, NSStackView, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSSize, NSString,
};

use vetter_core::known_hosts::KnownHostEntry;
use vetter_core::matcher::{Rule, Scope};
use vetter_core::suggest::{HostSuggestion, RuleSuggestion};
use vetter_core::wire::WireScope;

use crate::suggestions;
use crate::Context;

/// Width in points reserved for the picker accessory view. Tuned to
/// fit a moderately verbose YAML preview (one method, one host, a
/// path glob) without forcing the alert to balloon its width — the
/// `wrappingLabelWithString` preview below each radio wraps to this
/// width and the rest of the alert grows downward.
const ACCESSORY_WIDTH: f64 = 460.0;
/// Floor for the accessory height so an alert with a single short
/// tier doesn't render as an awkward 22pt strip. Anything taller
/// than this is driven by `fittingSize()`.
const ACCESSORY_MIN_HEIGHT: f64 = 60.0;

define_class!(
    /// Shared target for every radio inside a single picker accessory.
    ///
    /// AppKit's documented contract for radio-button auto-grouping
    /// (the behaviour where clicking one radio toggles its siblings
    /// off) is: every button in the group must share **both** a
    /// direct superview *and* the same action selector. This
    /// picker's layout wraps every `radio + preview` pair in its
    /// own per-row `NSStackView` (see [`build_picker_stack`]) so
    /// the radios end up in different superviews and AppKit's
    /// auto-grouping never kicks in — the prior version with a
    /// no-op `radioClicked:` left every radio independent, so
    /// users could light up multiple tiers at once and
    /// [`read_selected_index`] would return whichever radio
    /// happened to come first in `On` state instead of the user's
    /// actual choice.
    ///
    /// Rather than flatten the layout (which would force us to
    /// hand-manage spacing between radio/preview pairs), we
    /// enforce the invariant ourselves: the controller stores
    /// every radio in the group via [`set_radios`], and
    /// `radioClicked:` walks the list, turning the sender On and
    /// every sibling Off. Controller is held alive by the picker
    /// function's local binding for the duration of `runModal()`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VetterPickerRadioGroup"]
    #[ivars = RadioGroupIvars]
    pub(crate) struct RadioGroupController;

    unsafe impl NSObjectProtocol for RadioGroupController {}

    impl RadioGroupController {
        #[unsafe(method(radioClicked:))]
        fn radio_clicked(&self, sender: Option<&NSButton>) {
            let Some(sender) = sender else { return };
            let radios = self.ivars().radios.lock().expect("radios poisoned");
            for r in radios.iter() {
                if std::ptr::eq(&**r as *const NSButton, sender as *const NSButton) {
                    r.setState(NSControlStateValueOn);
                } else {
                    r.setState(NSControlStateValueOff);
                }
            }
        }
    }
);

/// Ivars for [`RadioGroupController`]. The controller needs a
/// handle to every radio in its group so `radioClicked:` can
/// turn off the siblings of the freshly-pressed radio — AppKit's
/// auto-grouping doesn't apply here, see the type-level comment.
/// `Mutex` mirrors the [`crate::runloop::popover::PopoverControllerIvars`]
/// pattern; in practice every access happens on the main thread
/// (the controller is `MainThreadOnly`) so contention is nil.
#[derive(Default)]
pub(crate) struct RadioGroupIvars {
    radios: Mutex<Vec<Retained<NSButton>>>,
}

impl RadioGroupController {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RadioGroupIvars::default());
        unsafe { objc2::msg_send![super(this), init] }
    }

    /// Register the radios that share this controller as their
    /// target. Called once per picker after the accessory view
    /// has finished building. Subsequent `radioClicked:` events
    /// walk this list to enforce the "exactly one On" invariant.
    fn set_radios(&self, radios: Vec<Retained<NSButton>>) {
        let mut g = self.ivars().radios.lock().expect("radios poisoned");
        *g = radios;
    }
}

/// Duration choices offered by the picker's second radio group.
/// Order here is the display order (top to bottom) and the default
/// selection is [`DurationChoice::Forever`] — this preserves the
/// pre-existing one-click muscle memory of "pick a tier, hit the
/// button, done" for anyone who never touches the duration group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationChoice {
    FifteenMinutes,
    OneHour,
    FourHours,
    /// Scoped to the requesting connection's stable POSIX session
    /// (see [`vetter_core::peer_cred::stable_session_for`]).
    /// Disabled in the UI when the card never resolved a `peer_sid`.
    ThisSession,
    Forever,
}

impl DurationChoice {
    const ALL: [DurationChoice; 5] = [
        DurationChoice::FifteenMinutes,
        DurationChoice::OneHour,
        DurationChoice::FourHours,
        DurationChoice::ThisSession,
        DurationChoice::Forever,
    ];

    fn title(&self) -> &'static str {
        match self {
            DurationChoice::FifteenMinutes => "15 minutes",
            DurationChoice::OneHour => "1 hour",
            DurationChoice::FourHours => "4 hours",
            DurationChoice::ThisSession => "For this terminal session",
            DurationChoice::Forever => "Forever",
        }
    }

    /// Backstop TTL applied to `Some(sid)`-scoped rules purely so
    /// `add_rule`'s lazy prune (see
    /// `vetter_core::matcher::loader::add_rule`) eventually reaps
    /// them even if the terminal that set the SID never comes back
    /// to invalidate the match. Does not change matching behaviour
    /// — the `sid` check already stops the rule from matching well
    /// before this elapses — it just bounds how long a dead entry
    /// can sit physically in the YAML file.
    const SESSION_BACKSTOP_SECS: u64 = 7 * 24 * 60 * 60;

    /// Compute the `(expires_at, sid)` pair to write onto the
    /// [`Rule`] for this choice, given the current wall clock and
    /// the card's recorded `peer_sid`. Only [`DurationChoice::ThisSession`]
    /// reads `peer_sid`; every other variant ignores it.
    fn apply(&self, now: u64, peer_sid: Option<i32>) -> (Option<u64>, Option<i32>) {
        match self {
            DurationChoice::FifteenMinutes => (Some(now + 15 * 60), None),
            DurationChoice::OneHour => (Some(now + 60 * 60), None),
            DurationChoice::FourHours => (Some(now + 4 * 60 * 60), None),
            DurationChoice::ThisSession => (Some(now + Self::SESSION_BACKSTOP_SECS), peer_sid),
            DurationChoice::Forever => (None, None),
        }
    }

    fn confirmation_note(&self) -> &'static str {
        match self {
            DurationChoice::FifteenMinutes => "Expires in 15 minutes.",
            DurationChoice::OneHour => "Expires in 1 hour.",
            DurationChoice::FourHours => "Expires in 4 hours.",
            DurationChoice::ThisSession => "Active for this terminal session.",
            DurationChoice::Forever => "Persisted to your user allowlist.",
        }
    }
}

/// Show the allowlist picker for `suggestions`. `peer_sid` is the
/// triggering request's recorded session id (see
/// [`crate::suggestions::suggestions_for`]); it's threaded through
/// so the "For this terminal session" duration choice can be
/// disabled (with an explanatory tooltip) when the daemon never
/// resolved one for this connection. Returns immediately after the
/// alert is dismissed; success / error follow-up alerts are
/// dispatched from the background persist closure.
pub fn show_allowlist_picker(
    mtm: MainThreadMarker,
    ctx: Arc<Context>,
    suggestions: Vec<RuleSuggestion>,
    peer_sid: Option<i32>,
) {
    if suggestions.is_empty() {
        // Defensive: caller should not have offered the button.
        return;
    }
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Add to allowlist"));
    alert.setInformativeText(ns_string!(
        "Pick a generalisation tier and a duration. The new rule is appended to your \
         user allowlist; pending requests it covers will be auto-approved."
    ));
    alert.setAlertStyle(NSAlertStyle::Informational);
    pin_dark_appearance(&alert);

    // `_group` / `_duration_group` keep their shared radio-button
    // targets alive for the duration of `runModal()`; without these
    // bindings they'd drop immediately and the buttons would lose
    // their grouping.
    let (tier_view, radios, _group) = build_radio_stack_for_rules(&suggestions, mtm);
    let (duration_view, duration_radios, _duration_group) =
        build_duration_stack(mtm, peer_sid.is_some());

    let accessory = NSStackView::new(mtm);
    accessory.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    accessory.setAlignment(NSLayoutAttribute::Leading);
    accessory.setSpacing(16.0);
    accessory.addArrangedSubview(&tier_view);
    let duration_label = NSTextField::labelWithString(ns_string!("Duration:"), mtm);
    accessory.addArrangedSubview(&duration_label);
    accessory.addArrangedSubview(&duration_view);
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[accessory
        .widthAnchor()
        .constraintEqualToConstant(ACCESSORY_WIDTH)]));
    let fitting = accessory.fittingSize();
    accessory.setFrameSize(NSSize::new(ACCESSORY_WIDTH, fitting.height));

    alert.setAccessoryView(Some(&accessory.into_super()));
    alert.addButtonWithTitle(ns_string!("Add to user allowlist"));
    alert.addButtonWithTitle(ns_string!("Cancel"));

    let response = alert.runModal();
    if response != NSAlertFirstButtonReturn {
        return;
    }
    let Some(picked) = read_selected_index(&radios) else {
        return;
    };
    let Some(suggestion) = suggestions.into_iter().nth(picked) else {
        return;
    };
    let duration = read_selected_index(&duration_radios)
        .and_then(|i| DurationChoice::ALL.get(i).copied())
        .unwrap_or(DurationChoice::Forever);
    let now = vetter_core::matcher::now_epoch_secs();
    let (expires_at, sid) = duration.apply(now, peer_sid);

    let mut rule = suggestion.rule;
    rule.expires_at = expires_at;
    rule.sid = sid;
    persist_rule_async(mtm, ctx, rule, duration.confirmation_note());
}

/// Build the "Duration:" radio group. `session_available` gates
/// whether the "For this terminal session" radio is interactive —
/// it's disabled with an explanatory tooltip when the triggering
/// card never resolved a `peer_sid` (headless / non-tty callers).
fn build_duration_stack(
    mtm: MainThreadMarker,
    session_available: bool,
) -> (
    Retained<NSView>,
    Vec<Retained<NSButton>>,
    Retained<RadioGroupController>,
) {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(6.0);

    let group = RadioGroupController::new(mtm);
    let group_target: &AnyObject = unsafe { &*(Retained::as_ptr(&group) as *const AnyObject) };
    let action = sel!(radioClicked:);

    let default_idx = DurationChoice::ALL
        .iter()
        .position(|d| *d == DurationChoice::Forever)
        .unwrap_or(0);

    let mut radios = Vec::with_capacity(DurationChoice::ALL.len());
    for (i, choice) in DurationChoice::ALL.iter().enumerate() {
        let radio = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(choice.title()),
                Some(group_target),
                Some(action),
                mtm,
            )
        };
        radio.setButtonType(NSButtonType::Radio);
        radio.setTag(i as isize);
        radio.setState(if i == default_idx {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        if *choice == DurationChoice::ThisSession && !session_available {
            unsafe {
                let _: () = objc2::msg_send![&*radio, setEnabled: false];
            }
            radio.setToolTip(Some(ns_string!(
                "No stable terminal session was recorded for this request."
            )));
        }
        radios.push(radio.clone());
        stack.addArrangedSubview(&radio);
    }
    group.set_radios(radios.clone());

    (stack.into_super(), radios, group)
}

/// Confirm + execute a `Revoke rule` action surfaced from the
/// "See approval reason" disclosure on an auto-allow Recent card.
///
/// Pops a destructive `NSAlert` ("Remove rule `<id>`…?") with the
/// matched scope spelled out so the user knows where the YAML
/// edit will land. On confirm, dispatches
/// [`suggestions::remove_allowlist_rule`] onto the global concurrent
/// queue so the file IO + reload doesn't stall the main thread, and
/// surfaces a follow-up `NSAlert` (success / error) on the main
/// queue. Past auto-allowed cards stay in the Recent ring as a
/// historical record — only future requests see the change.
pub fn confirm_and_revoke_rule(
    mtm: MainThreadMarker,
    ctx: Arc<Context>,
    rule_id: String,
    rule_scope: Scope,
) {
    // Today only `User`-scope rules are wired through the popover
    // (matches the `add_allowlist_rule` path). Session-scoped rules
    // (`expires_at`/`sid` set) now live in the *same* on-disk user
    // file as any other rule (see `vetter_core::matcher::loader`'s
    // load-time partition) — so `Scope::Session` is revokable via
    // the identical `WireScope::User` remove call, not a dead end.
    // A Recent card built from a built-in or denylist hit shouldn't
    // surface the Revoke button in the first place — but we defend
    // here so a stale popover snapshot can't trigger a misleading
    // "removed" alert.
    let scope = match rule_scope {
        Scope::User | Scope::Session => WireScope::User,
        Scope::Project | Scope::Builtin | Scope::Denylist => {
            let alert = NSAlert::new(mtm);
            pin_dark_appearance(&alert);
            alert.setMessageText(ns_string!("Cannot revoke from this scope"));
            alert.setInformativeText(&NSString::from_str(&format!(
                "Rule `{rule_id}` lives in the {} layer, which the popover does not edit. \
                 Use `vet allow rm` for project rules; built-in / denylist / session \
                 entries are not removable from the UI.",
                rule_scope.as_str()
            )));
            alert.setAlertStyle(NSAlertStyle::Informational);
            alert.addButtonWithTitle(ns_string!("OK"));
            let _ = alert.runModal();
            return;
        }
    };

    let confirm = NSAlert::new(mtm);
    pin_dark_appearance(&confirm);
    confirm.setMessageText(&NSString::from_str(&format!("Remove rule `{rule_id}`?")));
    confirm.setInformativeText(&NSString::from_str(&format!(
        "This deletes the rule from your user allowlist. \
         Past auto-allowed entries will stay in Recent as a record \
         of what was approved while the rule was active; only future \
         requests will be re-prompted.\n\nScope: {}",
        rule_scope.as_str()
    )));
    confirm.setAlertStyle(NSAlertStyle::Warning);
    // First button is the destructive action; macOS HIG would prefer
    // the cancel-as-default but `runModal()`'s convention is that
    // the first added button is the default and returns
    // `NSAlertFirstButtonReturn`. We add Cancel first to make it the
    // default, then the destructive action second so an accidental
    // Enter doesn't fire the revoke.
    confirm.addButtonWithTitle(ns_string!("Cancel"));
    confirm.addButtonWithTitle(ns_string!("Remove rule"));

    let response = confirm.runModal();
    // First button is Cancel; second is the destructive Remove.
    if response == NSAlertFirstButtonReturn {
        return;
    }

    revoke_rule_async(mtm, ctx, scope, rule_id);
}

/// Show the trust-host picker for `suggestions`.
pub fn show_host_picker(
    mtm: MainThreadMarker,
    ctx: Arc<Context>,
    suggestions: Vec<HostSuggestion>,
) {
    if suggestions.is_empty() {
        return;
    }
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Trust host"));
    alert.setInformativeText(ns_string!(
        "Pick a host pattern. Trusting a host removes the orange host pill on \
         pending and future cards but does not auto-approve any request."
    ));
    alert.setAlertStyle(NSAlertStyle::Informational);
    pin_dark_appearance(&alert);

    // See note in `show_allowlist_picker` re: `_group` lifetime.
    let (accessory, radios, _group) = build_radio_stack_for_hosts(&suggestions, mtm);
    alert.setAccessoryView(Some(&accessory));
    alert.addButtonWithTitle(ns_string!("Trust this host"));
    alert.addButtonWithTitle(ns_string!("Cancel"));

    let response = alert.runModal();
    if response != NSAlertFirstButtonReturn {
        return;
    }
    let Some(picked) = read_selected_index(&radios) else {
        return;
    };
    let Some(suggestion) = suggestions.into_iter().nth(picked) else {
        return;
    };
    persist_host_async(mtm, ctx, suggestion.entry);
}

fn build_radio_stack_for_rules(
    suggestions: &[RuleSuggestion],
    mtm: MainThreadMarker,
) -> (
    Retained<NSView>,
    Vec<Retained<NSButton>>,
    Retained<RadioGroupController>,
) {
    let rows: Vec<(String, String)> = suggestions
        .iter()
        .map(|s| {
            (
                format!("{}  —  {}", s.tier.as_str(), s.label),
                render_rule_when_yaml(&s.rule),
            )
        })
        .collect();
    build_picker_stack(&rows, mtm)
}

fn build_radio_stack_for_hosts(
    suggestions: &[HostSuggestion],
    mtm: MainThreadMarker,
) -> (
    Retained<NSView>,
    Vec<Retained<NSButton>>,
    Retained<RadioGroupController>,
) {
    let rows: Vec<(String, String)> = suggestions
        .iter()
        .map(|s| {
            (
                format!("{}  —  {}", s.tier.as_str(), s.label),
                format!("pattern: \"{}\"", s.entry.pattern),
            )
        })
        .collect();
    build_picker_stack(&rows, mtm)
}

/// Build the accessory stack shared by both pickers.
///
/// Each input pair is `(radio title, monospaced preview body)`.
/// Layout per row: a `buttonWithTitle:`-built radio sitting above
/// a `wrappingLabelWithString`-built multi-line label that
/// respects embedded newlines and wraps at `ACCESSORY_WIDTH`.
///
/// Critical layout choices — the prior version used
/// `NSStackViewDistribution::Fill` on a hard-coded oversize frame,
/// default `CenterX` alignment, `NSButton::new()`, and non-wrapping
/// `labelWithString`, which together produced misaligned rows with
/// one wildly stretched radio button:
///
/// - **Default distribution** (no `setDistribution` call) ensures
///   each row keeps its intrinsic height; Fill would split the
///   slack between children and squish the radio cells.
/// - **`Leading` alignment** on the outer + inner stacks pins
///   left edges so the radio and its preview don't get
///   centered relative to each other.
/// - **`buttonWithTitle:target:action:`** gives the radio a
///   properly-sized cell; bare `NSButton::new()` returned a
///   zero-frame button that Auto Layout couldn't measure.
/// - **`wrappingLabelWithString`** honours the YAML preview's
///   embedded newlines; `labelWithString` collapses to one line.
/// - Width is pinned via Auto Layout, height comes from
///   `fittingSize()` so the alert grows downward as needed.
fn build_picker_stack(
    rows: &[(String, String)],
    mtm: MainThreadMarker,
) -> (
    Retained<NSView>,
    Vec<Retained<NSButton>>,
    Retained<RadioGroupController>,
) {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(14.0);

    // Shared target+action for the whole group; see
    // `RadioGroupController` for why this is required.
    let group = RadioGroupController::new(mtm);
    let group_target: &AnyObject = unsafe { &*(Retained::as_ptr(&group) as *const AnyObject) };
    let action = sel!(radioClicked:);

    let mut radios = Vec::with_capacity(rows.len());
    for (i, (title, preview_body)) in rows.iter().enumerate() {
        let radio = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(group_target),
                Some(action),
                mtm,
            )
        };
        radio.setButtonType(NSButtonType::Radio);
        radio.setTag(i as isize);
        radio.setState(if i == 0 {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        radios.push(radio.clone());

        let preview = NSTextField::wrappingLabelWithString(&NSString::from_str(preview_body), mtm);
        preview.setFont(Some(
            &NSFont::userFixedPitchFontOfSize(11.0).unwrap_or(NSFont::systemFontOfSize(11.0)),
        ));
        preview.setSelectable(true);
        preview.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let row = NSStackView::new(mtm);
        row.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        row.setAlignment(NSLayoutAttribute::Leading);
        row.setSpacing(4.0);
        row.addArrangedSubview(&radio);
        row.addArrangedSubview(&preview);
        stack.addArrangedSubview(&row);
    }

    // Hand the radio list to the controller so `radioClicked:`
    // can enforce the group invariant; see the doc comment on
    // [`RadioGroupController`] for why AppKit's auto-grouping
    // doesn't apply to this layout.
    group.set_radios(radios.clone());

    // Pin width via Auto Layout so wrapping labels know what to
    // wrap at, then read the natural fitting height. NSAlert
    // positions the accessory by frame, so we set both dimensions
    // on the stack's frame after measuring.
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[stack
        .widthAnchor()
        .constraintEqualToConstant(ACCESSORY_WIDTH)]));
    let fitting = stack.fittingSize();
    stack.setFrameSize(NSSize::new(
        ACCESSORY_WIDTH,
        fitting.height.max(ACCESSORY_MIN_HEIGHT),
    ));

    (stack.into_super(), radios, group)
}

/// Reads which radio in `radios` carries `NSControlStateValueOn`
/// and returns its index. The grouping invariant (only one radio
/// On at a time) is enforced by [`RadioGroupController::radio_clicked`],
/// which sweeps siblings Off when a new radio is clicked.
/// Returns `None` only if every radio was somehow turned off
/// (defensive — should not happen in practice because the first
/// radio starts On and a click on the already-active radio leaves
/// it On via the same sweep).
fn read_selected_index(radios: &[Retained<NSButton>]) -> Option<usize> {
    let on = NSControlStateValueOn;
    radios.iter().position(|r| r.state() == on)
}

/// Render `rule.when` as a YAML snippet for the preview label. We
/// serialise just the `when:` block (not the whole rule) so the
/// preview reads as the user sees it in their allowlist, without
/// the noisy `id:` / `created_*:` autoderived fields.
fn render_rule_when_yaml(rule: &Rule) -> String {
    match serde_yaml_ng::to_string(&rule.when) {
        Ok(s) => format!("when:\n{}", indent_lines(&s, "  ")),
        Err(e) => format!("(yaml render failed: {e})"),
    }
}

fn indent_lines(s: &str, pad: &str) -> String {
    s.lines()
        .map(|l| format!("{pad}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pin_dark_appearance(alert: &NSAlert) {
    if let Some(dark) = unsafe { NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua) } {
        let window = alert.window();
        window.setAppearance(Some(&dark));
    }
}

/// Persist `rule` from a background dispatch and surface the result
/// on the main queue. `duration_note` is the picker's duration-
/// specific wording (e.g. "Expires in 15 minutes.") prepended to the
/// success alert; the underlying persist call is identical
/// regardless of duration (see [`DurationChoice::apply`]). The alert
/// is fire-and-forget; we don't wait for it to be dismissed before
/// returning to the caller.
fn persist_rule_async(mtm: MainThreadMarker, ctx: Arc<Context>, rule: Rule, duration_note: &str) {
    let _ = mtm; // captured for type checking; persist hops back to main below.
    let duration_note = duration_note.to_string();
    DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(
        DispatchQoS::Default,
    ))
    .exec_async(move || {
        let result = suggestions::add_allowlist_rule(&ctx, WireScope::User, rule);
        DispatchQueue::main().exec_async(move || {
            let mtm =
                MainThreadMarker::new().expect("post-persist alert dispatched onto main queue");
            let alert = NSAlert::new(mtm);
            pin_dark_appearance(&alert);
            match result {
                Ok(added) => {
                    // Auto-approved entries are about to leave
                    // the pending queue; clear any delivered
                    // banner for them so Notification Center
                    // doesn't keep a stale card around. Same
                    // hygiene the Approve/Reject branches do
                    // in `did_receive_response`. Also matters
                    // for the new banner-side `Allowlist…`
                    // action, but the popover-driven path
                    // benefits too — picking an Allowlist…
                    // tier from the popover used to leave the
                    // matching banner behind.
                    if !added.auto_approved_ids.is_empty() {
                        super::remove_delivered_for_ids(added.auto_approved_ids.clone());
                    }
                    alert.setMessageText(ns_string!("Rule added"));
                    let body = if added.auto_approved_ids.is_empty() {
                        format!(
                            "Persisted as `{}`. {duration_note} No pending requests matched.",
                            added.id
                        )
                    } else {
                        format!(
                            "Persisted as `{}`. {duration_note} Auto-approved {} pending request(s).",
                            added.id,
                            added.auto_approved_ids.len()
                        )
                    };
                    alert.setInformativeText(&NSString::from_str(&body));
                    alert.setAlertStyle(NSAlertStyle::Informational);
                }
                Err(e) => {
                    alert.setMessageText(ns_string!("Could not add rule"));
                    alert.setInformativeText(&NSString::from_str(&e.to_string()));
                    alert.setAlertStyle(NSAlertStyle::Warning);
                }
            }
            alert.addButtonWithTitle(ns_string!("OK"));
            let _ = alert.runModal();
        });
    });
}

/// Background-dispatched sibling of [`persist_rule_async`] that
/// removes (rather than appends) an allowlist rule. Mirrors the
/// async hop pattern so the file-IO + YAML reload stays off the
/// main thread, and surfaces a follow-up `NSAlert` on the main
/// queue describing what happened.
fn revoke_rule_async(mtm: MainThreadMarker, ctx: Arc<Context>, scope: WireScope, id: String) {
    let _ = mtm; // captured for type checking; alert hops back to main below.
    DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(
        DispatchQoS::Default,
    ))
    .exec_async(move || {
        let result = suggestions::remove_allowlist_rule(&ctx, scope, &id);
        DispatchQueue::main().exec_async(move || {
            let mtm =
                MainThreadMarker::new().expect("post-revoke alert dispatched onto main queue");
            let alert = NSAlert::new(mtm);
            pin_dark_appearance(&alert);
            match result {
                Ok(()) => {
                    alert.setMessageText(ns_string!("Rule removed"));
                    alert.setInformativeText(&NSString::from_str(&format!(
                        "Rule `{id}` has been removed from your user allowlist."
                    )));
                    alert.setAlertStyle(NSAlertStyle::Informational);
                }
                Err(e) => {
                    alert.setMessageText(ns_string!("Could not remove rule"));
                    alert.setInformativeText(&NSString::from_str(&e.to_string()));
                    alert.setAlertStyle(NSAlertStyle::Warning);
                }
            }
            alert.addButtonWithTitle(ns_string!("OK"));
            let _ = alert.runModal();
        });
    });
}

fn persist_host_async(mtm: MainThreadMarker, ctx: Arc<Context>, entry: KnownHostEntry) {
    let _ = mtm;
    DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(
        DispatchQoS::Default,
    ))
    .exec_async(move || {
        let pattern = entry.pattern.clone();
        let result = suggestions::add_known_host(&ctx, WireScope::User, entry);
        DispatchQueue::main().exec_async(move || {
            let mtm =
                MainThreadMarker::new().expect("post-persist alert dispatched onto main queue");
            let alert = NSAlert::new(mtm);
            pin_dark_appearance(&alert);
            match result {
                Ok(()) => {
                    alert.setMessageText(ns_string!("Host trusted"));
                    alert.setInformativeText(&NSString::from_str(&format!(
                        "Pattern `{pattern}` added to your user known-hosts."
                    )));
                    alert.setAlertStyle(NSAlertStyle::Informational);
                }
                Err(e) => {
                    alert.setMessageText(ns_string!("Could not trust host"));
                    alert.setInformativeText(&NSString::from_str(&e.to_string()));
                    alert.setAlertStyle(NSAlertStyle::Warning);
                }
            }
            alert.addButtonWithTitle(ns_string!("OK"));
            let _ = alert.runModal();
        });
    });
}
