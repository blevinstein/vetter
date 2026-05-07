//! Smart URL row for the approver popover.
//!
//! Replaces the single `command  VERB url` header label with a
//! horizontal stack of typed tokens so a glance answers "where is
//! the agent reaching, and is that host familiar?":
//!
//! ```text
//! [GET]  https://  [api.example.com]  :8080  /v1/things  ?q=1
//! ```
//!
//! See [plans/ApprovalUI.md "URL row"](../../../plans/ApprovalUI.md)
//! for the token-by-token style table. The host pill recipe shares
//! the [`crate::runloop::popover_pills`] tinted-pill builder so the
//! green/orange palette matches the signal pills below.
//!
//! Non-HttpRequest cards (today: nothing; tomorrow: a future
//! `ProcessSpawn`-only request) fall back to a plain
//! "<command>  VERB target" label so the slot is always populated.

#![cfg(target_os = "macos")]

use objc2::rc::Retained;
use objc2_app_kit::{
    NSColor, NSFont, NSFontWeightSemibold, NSStackView, NSStackViewDistribution, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{MainThreadMarker, NSString};
use vetter_core::render::sanitize_for_display;
use vetter_core::{HttpMethod, HttpRequest};

use super::popover_pills;

/// Body font size for non-pill URL tokens. Picked to match the
/// monospaced semibold header font we use elsewhere.
const URL_FONT_SIZE: f64 = 13.0;

/// "Standard" ports we silently drop instead of rendering as
/// `:443`. 8080 / 8443 are listed because they're common in dev
/// stacks and a non-default warning would be noisy.
const QUIET_PORTS: &[u16] = &[80, 443, 8080, 8443];

/// Upper bound on the width of the path/query wrapping label. The
/// label sits inside a card that is ultimately width-pinned to
/// `POPOVER_WIDTH - CARDS_HORIZONTAL_MARGIN*2 - CARD_INSET*2` in
/// `popover.rs`, minus the bold `curl` command-label token the
/// header row leads with. Rather than thread those constants across
/// modules we budget a conservative slack here: the label
/// `preferredMaxLayoutWidth` is used only to tell AppKit *when* to
/// start wrapping (if the real available width is narrower the
/// label still wraps to fit, via the card's own width pin). Tuning
/// target: a realistic Datadog-style query (3+ `filter[*]`
/// segments) that blew past the row before this refactor now wraps
/// to two lines and keeps the card inside the popover width.
const URL_PATH_MAX_WIDTH: f64 = 420.0;

/// Build the URL row for a `HttpRequest`.
///
/// `host_known` answers "did the daemon's `KnownHostsStore` (or the
/// loopback rule) recognise `req.url.host_str()`?" — used to pick
/// between green and orange pill tints.
///
/// Returns a **vertical** stack of 1–2 rows:
///
/// - **Top row** (always): method badge + scheme + host pill +
///   optional non-standard port. These tokens are always short, so
///   they keep a consistent single-line layout regardless of URL
///   complexity.
/// - **Bottom row** (only when the URL has a non-root path or a
///   query string): a wrapping monospaced label carrying the path +
///   `?query` string. `wrappingLabelWithString` combined with a
///   `preferredMaxLayoutWidth` constraint lets long URLs wrap to
///   two or three lines instead of blowing out the card width —
///   which previously caused the dreaded "the URL ran off the
///   right edge of the popover" bug on Datadog-style
///   `--data-urlencode` curls.
pub fn build_url_row(
    req: &HttpRequest,
    host_known: bool,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    // Outer container: vertical so the optional path/query line
    // drops below the method/scheme/host row rather than competing
    // with it for horizontal real estate. The perpendicular
    // alignment is `Leading` so the path line sits flush under the
    // method token above — scanning the left edge traces the URL
    // top-to-bottom.
    let outer = NSStackView::new(mtm);
    outer.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    outer.setSpacing(2.0);
    outer.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
    outer.setDistribution(NSStackViewDistribution::Fill);

    // Top row — short, single-line tokens.
    let top = NSStackView::new(mtm);
    top.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    top.setSpacing(6.0);
    top.setDistribution(NSStackViewDistribution::Fill);

    // 1. Method badge — bold, colored per `HttpMethodColour`.
    let method_str = sanitize_for_display(req.method.as_str());
    let method_label =
        NSTextField::labelWithString(&NSString::from_str(&format!("[{method_str}]")), mtm);
    let semibold = unsafe { NSFontWeightSemibold };
    method_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
        URL_FONT_SIZE,
        semibold,
    )));
    method_label.setTextColor(Some(&method_color(&req.method)));
    top.addArrangedSubview(&method_label);

    // 2. Scheme — `https://` etc. Dim so the eye lands on the host.
    let scheme = format!("{}://", sanitize_for_display(req.url.scheme()));
    let scheme_label = NSTextField::labelWithString(&NSString::from_str(&scheme), mtm);
    scheme_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
        URL_FONT_SIZE,
        0.0,
    )));
    scheme_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    top.addArrangedSubview(&scheme_label);

    // 3. Host pill — trust-coloured. Loopback gets a third class
    //    ("trusted local"); known/unknown otherwise. We carry the
    //    trust class into the tooltip so hover explains the colour.
    let host = req.url.host_str().unwrap_or("");
    let safe_host = sanitize_for_display(host);
    let trust = host_trust(host, host_known);
    let host_pill =
        popover_pills::build_pill(&safe_host, &trust.fg(), &trust.bg(), trust.tooltip(), mtm);
    top.addArrangedSubview(&host_pill);

    // 4. `:port` if present and non-quiet.
    if let Some(port) = req.url.port() {
        if !QUIET_PORTS.contains(&port) {
            let port_label =
                NSTextField::labelWithString(&NSString::from_str(&format!(":{port}")), mtm);
            port_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
                URL_FONT_SIZE,
                0.0,
            )));
            // Non-standard ports are a soft "look here" cue; orange
            // mirrors the `non-standard-port` Warn pill.
            port_label.setTextColor(Some(&NSColor::systemOrangeColor()));
            top.addArrangedSubview(&port_label);
        }
    }

    // Trailing flexible spacer so top-row tokens bunch left rather
    // than stretching to fill the popover width.
    let top_spacer = NSView::new(mtm);
    top.addArrangedSubview(&top_spacer);
    outer.addArrangedSubview(&top);

    // 5 + 6. Path + query on a second wrapping row. We fold both
    //        into a single `wrappingLabelWithString` so the wrap
    //        boundary can fall inside a long path *or* inside a
    //        long query without needing two separate sizing passes.
    //        The row is omitted entirely when the URL has no
    //        meaningful path/query (`https://host` or
    //        `https://host/` with no `?…`) — leaving just the
    //        top-row tokens, which is what the single-line
    //        variant showed before this refactor.
    let path = req.url.path();
    let query = req.url.query();
    let has_path = !path.is_empty() && path != "/";
    let has_query = query.is_some();

    // Build the path+query string. The root case (`path == "/"`)
    // is kept visible as a dim `/` only when it's the full URL
    // target (no query) — otherwise the query label leads with
    // `?` and the `/` would be redundant.
    let path_query = if has_path && has_query {
        format!("{path}?{}", query.unwrap())
    } else if has_path {
        path.to_string()
    } else if has_query {
        format!("/?{}", query.unwrap())
    } else if path == "/" {
        "/".to_string()
    } else {
        String::new()
    };

    if !path_query.is_empty() {
        let tail = NSTextField::wrappingLabelWithString(
            &NSString::from_str(&sanitize_for_display(&path_query)),
            mtm,
        );
        tail.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            URL_FONT_SIZE,
            0.0,
        )));
        // Query-only / bare-slash cases render dim so the token
        // visually tracks its meaning (`/?q=1` is a search, not a
        // navigational path). A concrete path renders in the
        // default foreground.
        if !has_path {
            tail.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }
        // Cap the preferred width so AppKit's layout engine knows
        // where to wrap. The card itself is further constrained
        // downstream (`add_full_width_arranged` in popover.rs), so
        // this bound is just a "start wrapping no later than
        // here" hint — real overflow beyond the card width is
        // prevented by the card pin.
        tail.setPreferredMaxLayoutWidth(URL_PATH_MAX_WIDTH);
        outer.addArrangedSubview(&tail);
    }

    outer.into_super()
}

/// Build a fallback URL row for non-HttpRequest cards (or for
/// callers that didn't ship a `parsed` body). Keeps the same slot
/// in the card layout populated so the visual cadence is constant.
///
/// Uses `wrappingLabelWithString` + `preferredMaxLayoutWidth` so a
/// long target (e.g. a `ssh user@really-long-hostname:path`) wraps
/// instead of blowing out the card width — symmetric with the
/// wrapping path/query tail in `build_url_row`.
pub fn build_fallback_row(
    command: &str,
    primary_verb: &str,
    primary_target: &str,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let safe_command = sanitize_for_display(command);
    let safe_verb = sanitize_for_display(primary_verb);
    let safe_target = sanitize_for_display(primary_target);
    let text = if primary_verb.is_empty() {
        format!("{safe_command}  {safe_target}")
    } else {
        format!("{safe_command}  {safe_verb} {safe_target}")
    };
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(&text), mtm);
    let semibold = unsafe { NSFontWeightSemibold };
    label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
        URL_FONT_SIZE,
        semibold,
    )));
    label.setPreferredMaxLayoutWidth(URL_PATH_MAX_WIDTH);
    label.into_super().into_super()
}

/// Pick a `HttpMethodColour` analogue for the AppKit row. The CLI
/// renderer uses `Style::Method(*)`; the popover repeats the same
/// taxonomy so a glance reads the same colour on both surfaces.
fn method_color(method: &HttpMethod) -> Retained<NSColor> {
    match method {
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Options | HttpMethod::Trace => {
            NSColor::systemGreenColor()
        }
        HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch => NSColor::systemYellowColor(),
        HttpMethod::Delete => NSColor::systemRedColor(),
        HttpMethod::Connect | HttpMethod::Other(_) => NSColor::systemPurpleColor(),
    }
}

/// One of the three host-trust classes used by the URL row's host
/// pill. Loopback wins over store-known (any loopback host is
/// always "trusted local" regardless of the store).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostTrust {
    /// Loopback (`localhost`, `127.0.0.1`, `[::1]`).
    Loopback,
    Known,
    Unknown,
}

impl HostTrust {
    fn fg(&self) -> Retained<NSColor> {
        match self {
            // Teal (not secondaryLabel) keeps the loopback pill
            // readable on the popover's dark surface while still
            // distinguishing it from the green known-host class.
            HostTrust::Loopback => NSColor::systemTealColor(),
            HostTrust::Known => NSColor::systemGreenColor(),
            HostTrust::Unknown => NSColor::systemOrangeColor(),
        }
    }

    fn bg(&self) -> Retained<NSColor> {
        // Each trust class is tinted with its own foreground at
        // low alpha (`popover_pills::pill_bg_for`) so the pill
        // reads as a soft tinted capsule on the dark popover
        // surface. The popover's appearance is pinned to Dark
        // Aqua in `popover.rs`, so this looks identical in Light
        // and Dark system themes.
        popover_pills::pill_bg_for(&self.fg())
    }

    fn tooltip(&self) -> &'static str {
        match self {
            HostTrust::Loopback => "loopback (trusted local)",
            HostTrust::Known => "known host (matched known-hosts list)",
            HostTrust::Unknown => "unknown host (not in known-hosts list)",
        }
    }
}

/// Classify `host` into one of three trust tiers. `host_known` is
/// the daemon's pre-computed answer for "store recognised"; the
/// loopback check is repeated here so a stale `host_known=false`
/// for `localhost` still paints loopback.
fn host_trust(host: &str, host_known: bool) -> HostTrust {
    if is_loopback(host) {
        HostTrust::Loopback
    } else if host_known {
        HostTrust::Known
    } else {
        HostTrust::Unknown
    }
}

/// Loopback rule mirrors `policy::is_known_host` and
/// `vetter_core::signals::is_loopback_host`.
fn is_loopback(host: &str) -> bool {
    use std::net::IpAddr;
    use std::str::FromStr;
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    IpAddr::from_str(stripped)
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "../tests/popover_url.rs"]
mod tests;
