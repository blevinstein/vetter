//! URL row segmentation and host-trust classification.
//!
//! The URL row breaks a request into typed tokens so a glance
//! answers "where is the agent reaching, and is that host
//! familiar?":
//!
//! ```text
//! [GET]  https://  [api.example.com]  :8080  /v1/things  ?q=1
//! ```
//!
//! Which tokens appear, and what each one *means*, is the same on
//! every platform: whether a port is worth showing at all, whether
//! the path and query collapse onto one line, and which of three
//! trust classes the host falls into. Only the drawing differs, so
//! only the drawing stays platform-side.
//!
//! See [plans/ApprovalUI.md "URL row"](../../../plans/ApprovalUI.md)
//! for the token-by-token style table.

use vetter_core::render::sanitize_for_display;
use vetter_core::HttpMethod;

/// "Standard" ports we silently drop instead of rendering as
/// `:443`. 8080 / 8443 are listed because they're common in dev
/// stacks and a non-default warning would be noisy.
const QUIET_PORTS: &[u16] = &[80, 443, 8080, 8443];

/// Semantic class of an HTTP method, mirroring the CLI renderer's
/// `Style::Method(*)` taxonomy so a glance reads the same on both
/// surfaces. A tone rather than a colour so each platform picks its
/// own palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodTone {
    /// Green. Safe / read-only verbs.
    Read,
    /// Yellow. Verbs that mutate server state.
    Write,
    /// Red. `DELETE`.
    Destructive,
    /// Purple. `CONNECT` and anything non-standard.
    Other,
}

/// Classify `method` into its display tone.
pub fn method_tone(method: &HttpMethod) -> MethodTone {
    match method {
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Options | HttpMethod::Trace => {
            MethodTone::Read
        }
        HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch => MethodTone::Write,
        HttpMethod::Delete => MethodTone::Destructive,
        HttpMethod::Connect | HttpMethod::Other(_) => MethodTone::Other,
    }
}

/// The port to render after the host, or `None` when the URL either
/// carries no explicit port or carries one boring enough to hide
/// (see [`QUIET_PORTS`]). A visible port is a soft "look here" cue
/// and pairs with the `non-standard-port` Warn pill.
pub fn visible_port(url: &url::Url) -> Option<u16> {
    url.port().filter(|p| !QUIET_PORTS.contains(p))
}

/// The path + query tail of the URL row, already sanitised for
/// display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathQuery {
    /// Text to paint, or empty when the row should be omitted
    /// entirely (`https://host` with no path and no query).
    pub text: String,
    /// True when the URL carries a real path (not just `/`). Drives
    /// the dim treatment: a query-only or bare-slash tail reads as
    /// "this is a search, not a location", so it renders muted while
    /// a concrete path renders in the default foreground.
    pub has_path: bool,
}

/// Fold a URL's path and query into the single wrapping tail the
/// URL row paints beneath the host.
///
/// Both halves go into one string so a wrap can fall inside a long
/// path *or* inside a long query without needing two sizing passes —
/// the fix for Datadog-style `--data-urlencode` URLs running off the
/// right edge of the card.
///
/// The root case (`path == "/"`) survives as a dim `/` only when
/// it's the whole target; with a query present the tail leads with
/// `?` and the slash would be redundant.
pub fn path_query(url: &url::Url) -> PathQuery {
    let path = url.path();
    let query = url.query();
    let has_path = !path.is_empty() && path != "/";
    let text = match (has_path, query) {
        (true, Some(q)) => format!("{path}?{q}"),
        (true, None) => path.to_string(),
        (false, Some(q)) => format!("/?{q}"),
        (false, None) if path == "/" => "/".to_string(),
        (false, None) => String::new(),
    };
    PathQuery {
        text: sanitize_for_display(&text).into_owned(),
        has_path,
    }
}

/// One of the three host-trust classes used by the URL row's host
/// pill. Loopback wins over store-known (any loopback host is
/// always "trusted local" regardless of the store).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostTrust {
    /// Loopback (`localhost`, `127.0.0.1`, `[::1]`).
    Loopback,
    Known,
    Unknown,
}

impl HostTrust {
    /// Hover text explaining why the pill is the colour it is.
    pub fn tooltip(&self) -> &'static str {
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
pub fn host_trust(host: &str, host_known: bool) -> HostTrust {
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
pub fn is_loopback(host: &str) -> bool {
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

/// Text for the fallback row painted on non-`HttpRequest` cards (or
/// for callers that didn't ship a `parsed` body). Keeps the URL-row
/// slot populated so the card's visual cadence is constant.
///
/// Every component is sanitised here rather than at the call site,
/// so no surface can accidentally paint raw argv bytes: a target
/// carrying RTLO or control characters would otherwise be able to
/// reorder what the human reads before approving.
pub fn fallback_text(command: &str, primary_verb: &str, primary_target: &str) -> String {
    let safe_command = sanitize_for_display(command);
    let safe_verb = sanitize_for_display(primary_verb);
    let safe_target = sanitize_for_display(primary_target);
    if primary_verb.is_empty() {
        format!("{safe_command}  {safe_target}")
    } else {
        format!("{safe_command}  {safe_verb} {safe_target}")
    }
}

#[cfg(test)]
#[path = "../tests/cards_url.rs"]
mod tests;
