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

use std::sync::Arc;

use dispatch2::{DispatchQoS, DispatchQueue, GlobalQueueIdentifier};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, sel, MainThreadOnly};
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
use vetter_core::matcher::Rule;
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
    /// off) is: every button in the group must share a superview
    /// **and** the same action selector. Bare `NSButton::Radio`
    /// instances with `target=nil, action=nil` (which is what
    /// `buttonWithTitle:target:action:` with `None`/`None` produces)
    /// are independent toggles — so the prior version of the picker
    /// let users light up every radio simultaneously.
    ///
    /// Wiring `target = some shared controller, action = radioClicked:`
    /// satisfies the grouping contract. The selector body is a
    /// deliberate no-op because AppKit performs the OFF-toggling
    /// itself before delivering the action; we only need the
    /// selector to *exist* so the responder-chain delivery doesn't
    /// log "no such selector" warnings into Console. The controller
    /// is otherwise stateless and is held alive by the picker
    /// function's local for the duration of `runModal()`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VetterPickerRadioGroup"]
    #[ivars = RadioGroupIvars]
    pub(crate) struct RadioGroupController;

    unsafe impl NSObjectProtocol for RadioGroupController {}

    impl RadioGroupController {
        #[unsafe(method(radioClicked:))]
        fn radio_clicked(&self, _sender: Option<&NSButton>) {}
    }
);

/// Empty ivar struct. `define_class!` requires `set_ivars(...)`
/// before sending `init` to super; the class itself is stateless
/// because the radio invariant is enforced entirely by AppKit.
#[derive(Default)]
pub(crate) struct RadioGroupIvars;

impl RadioGroupController {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RadioGroupIvars);
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// Show the allowlist picker for `suggestions`. Returns immediately
/// after the alert is dismissed; success / error follow-up alerts
/// are dispatched from the background persist closure.
pub fn show_allowlist_picker(
    mtm: MainThreadMarker,
    ctx: Arc<Context>,
    suggestions: Vec<RuleSuggestion>,
) {
    if suggestions.is_empty() {
        // Defensive: caller should not have offered the button.
        return;
    }
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("Add to allowlist"));
    alert.setInformativeText(ns_string!(
        "Pick a generalisation tier. The new rule is appended to your user allowlist; \
         pending requests it covers will be auto-approved."
    ));
    alert.setAlertStyle(NSAlertStyle::Informational);
    pin_dark_appearance(&alert);

    // `_group` keeps the shared radio-button target alive for the
    // duration of `runModal()`; without this binding it would drop
    // immediately and the buttons would lose their grouping.
    let (accessory, radios, _group) = build_radio_stack_for_rules(&suggestions, mtm);
    alert.setAccessoryView(Some(&accessory));
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
    persist_rule_async(mtm, ctx, suggestion.rule);
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
/// On at a time) is enforced by AppKit because every radio in a
/// picker shares a superview *and* the action selector wired up
/// in `build_picker_stack`. Returns `None` only if every radio
/// was somehow turned off (defensive — should not happen in
/// practice because the first radio starts On and AppKit refuses
/// to turn the active one off via a click).
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
/// on the main queue. The alert is fire-and-forget; we don't wait
/// for it to be dismissed before returning to the caller.
fn persist_rule_async(mtm: MainThreadMarker, ctx: Arc<Context>, rule: Rule) {
    let _ = mtm; // captured for type checking; persist hops back to main below.
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
                        format!("Persisted as `{}`. No pending requests matched.", added.id)
                    } else {
                        format!(
                            "Persisted as `{}`. Auto-approved {} pending request(s).",
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

/// When a radio in a picker stack is clicked, walk its sibling
/// radios and turn off every one that isn't the sender. NSAlert's
/// auto-grouping only kicks in when the radios share an action
/// selector — we wire each radio's action back to this function so
/// we can keep the grouping behaviour without giving every alert
/// its own subclassed controller object. Implemented as a free
/// function so the popover module's `define_class!` action selector
/// can call it without owning any state. Currently unused (we read
/// `state()` directly off each radio at dismissal time, see
/// `read_selected_index`); kept here as the documented extension
/// point if a future picker needs the live "exactly one selected"
/// invariant during interaction.
#[allow(dead_code)]
pub fn enforce_radio_group(radios: &[Retained<NSButton>], sender: &NSButton) {
    let off = NSControlStateValueOff;
    let on = NSControlStateValueOn;
    for r in radios {
        if std::ptr::eq(&**r, sender) {
            r.setState(on);
        } else {
            r.setState(off);
        }
    }
}
