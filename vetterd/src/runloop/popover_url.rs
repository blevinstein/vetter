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
use vetter_core::{HttpMethod, HttpRequest};

use super::popover_pills;

/// Body font size for non-pill URL tokens. Picked to match the
/// monospaced semibold header font we use elsewhere.
const URL_FONT_SIZE: f64 = 13.0;

/// "Standard" ports we silently drop instead of rendering as
/// `:443`. 8080 / 8443 are listed because they're common in dev
/// stacks and a non-default warning would be noisy.
const QUIET_PORTS: &[u16] = &[80, 443, 8080, 8443];

/// Build the URL row for a `HttpRequest`.
///
/// `host_known` answers "did the daemon's `KnownHostsStore` (or the
/// loopback rule) recognise `req.url.host_str()`?" — used to pick
/// between green and orange pill tints.
pub fn build_url_row(
    req: &HttpRequest,
    host_known: bool,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setSpacing(6.0);
    row.setDistribution(NSStackViewDistribution::Fill);

    // 1. Method badge — bold, colored per `HttpMethodColour`.
    let method_str = req.method.as_str();
    let method_label =
        NSTextField::labelWithString(&NSString::from_str(&format!("[{method_str}]")), mtm);
    let semibold = unsafe { NSFontWeightSemibold };
    method_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
        URL_FONT_SIZE,
        semibold,
    )));
    method_label.setTextColor(Some(&method_color(&req.method)));
    row.addArrangedSubview(&method_label);

    // 2. Scheme — `https://` etc. Dim so the eye lands on the host.
    let scheme = format!("{}://", req.url.scheme());
    let scheme_label = NSTextField::labelWithString(&NSString::from_str(&scheme), mtm);
    scheme_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
        URL_FONT_SIZE,
        0.0,
    )));
    scheme_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    row.addArrangedSubview(&scheme_label);

    // 3. Host pill — trust-coloured. Loopback gets a third class
    //    ("trusted local"); known/unknown otherwise. We carry the
    //    trust class into the tooltip so hover explains the colour.
    let host = req.url.host_str().unwrap_or("");
    let trust = host_trust(host, host_known);
    let host_pill = popover_pills::build_pill(host, &trust.fg(), &trust.bg(), trust.tooltip(), mtm);
    row.addArrangedSubview(&host_pill);

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
            row.addArrangedSubview(&port_label);
        }
    }

    // 5. Path. Default colour, monospaced regular. Empty paths
    //    (`https://example.test`) skip this slot.
    let path = req.url.path();
    if !path.is_empty() && path != "/" {
        let path_label = NSTextField::labelWithString(&NSString::from_str(path), mtm);
        path_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            URL_FONT_SIZE,
            0.0,
        )));
        row.addArrangedSubview(&path_label);
    } else if path == "/" {
        // Show the bare `/` so the row reads as a complete URL
        // rather than appearing truncated mid-token.
        let slash = NSTextField::labelWithString(&NSString::from_str("/"), mtm);
        slash.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            URL_FONT_SIZE,
            0.0,
        )));
        slash.setTextColor(Some(&NSColor::secondaryLabelColor()));
        row.addArrangedSubview(&slash);
    }

    // 6. Query summary. Per-param highlighting is future work; for
    //    now we render the raw `?key=value` string in dim
    //    monospaced. The `build_query_summary` helper is the
    //    extension point.
    if let Some(query) = req.url.query() {
        let q = build_query_summary(query);
        let q_label = NSTextField::labelWithString(&NSString::from_str(&q), mtm);
        q_label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            URL_FONT_SIZE,
            0.0,
        )));
        q_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        row.addArrangedSubview(&q_label);
    }

    // Trailing flexible spacer so tokens bunch left rather than
    // stretching to fill the popover width.
    let spacer = NSView::new(mtm);
    row.addArrangedSubview(&spacer);

    row.into_super()
}

/// Build a fallback URL row for non-HttpRequest cards (or for
/// callers that didn't ship a `parsed` body). Keeps the same slot
/// in the card layout populated so the visual cadence is constant.
pub fn build_fallback_row(
    command: &str,
    primary_verb: &str,
    primary_target: &str,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let text = if primary_verb.is_empty() {
        format!("{command}  {primary_target}")
    } else {
        format!("{command}  {primary_verb} {primary_target}")
    };
    let label = NSTextField::labelWithString(&NSString::from_str(&text), mtm);
    let semibold = unsafe { NSFontWeightSemibold };
    label.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
        URL_FONT_SIZE,
        semibold,
    )));
    label.into_super().into_super()
}

/// Render the query-string portion of a URL ready for display.
///
/// v1 just prefixes a `?`. The function is kept as its own public
/// hook so future per-param coloring (`api_key=...` red, ordinary
/// keys dim) can slot in without restructuring the URL row.
pub fn build_query_summary(query: &str) -> String {
    format!("?{query}")
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
