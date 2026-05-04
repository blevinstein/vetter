//! Tests for [`crate::runloop::popover_url`].
//!
//! AppKit-touching helpers (`build_url_row`, `build_fallback_row`)
//! get bare smoke tests that just check we can construct the views
//! without panicking on the main thread; the per-token visual
//! styling is verified manually in the Phase 4 smoke runs.
//!
//! The pure-Rust helpers (`build_query_summary`, the `host_trust`
//! classifier surfaced via `build_url_row`'s tooltip) get focused
//! unit tests so future per-param coloring or trust-class changes
//! land in CI rather than in screenshots.

#![cfg(target_os = "macos")]

use super::*;

#[test]
fn host_trust_loopback_wins_over_known_flag() {
    // The loopback rule must fire even if the daemon's
    // `host_known` hint says "false" — a stale store should never
    // paint `localhost` as scary orange.
    assert_eq!(host_trust("localhost", false), HostTrust::Loopback);
    assert_eq!(host_trust("LOCALHOST", false), HostTrust::Loopback);
    assert_eq!(host_trust("127.0.0.1", false), HostTrust::Loopback);
    assert_eq!(host_trust("[::1]", false), HostTrust::Loopback);
    // Even if the store does say known, loopback still classifies
    // as loopback (so the dim grey pill renders, not the green one).
    assert_eq!(host_trust("localhost", true), HostTrust::Loopback);
}

#[test]
fn host_trust_routes_known_and_unknown() {
    assert_eq!(host_trust("api.example.com", true), HostTrust::Known);
    assert_eq!(host_trust("never.heard.test", false), HostTrust::Unknown);
}

#[test]
fn is_loopback_matches_ipv4_and_ipv6_literals() {
    assert!(is_loopback("localhost"));
    assert!(is_loopback("127.0.0.1"));
    assert!(is_loopback("127.255.255.254"));
    assert!(is_loopback("[::1]"));
    assert!(is_loopback("::1"));
    assert!(!is_loopback("8.8.8.8"));
    assert!(!is_loopback("example.com"));
}

#[test]
fn build_url_row_smoke_runs_without_panicking() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let req = vetter_core::HttpRequest {
        method: vetter_core::HttpMethod::Get,
        url: url::Url::parse("https://api.example.com:8443/v1/things?q=1").unwrap(),
        headers: vec![],
        body: vetter_core::Body::None,
        auth: None,
        tls: vetter_core::TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    };
    let view = build_url_row(&req, true, mtm);
    let _ = &*view;
}

#[test]
fn build_fallback_row_smoke_runs_without_panicking() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let view = build_fallback_row("ssh", "ssh", "user@host", mtm);
    let _ = &*view;
}

#[test]
fn build_url_row_smoke_handles_long_urls() {
    // Long path + query (Datadog-style `--data-urlencode` curl) must
    // build without panicking; the wrapping-label path exists to
    // keep this case from overflowing the card width.
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let long_url = "https://api.datadoghq.com/api/v2/spans/events\
        ?filter[query]=service:corelab-case-mgmt-portal env:prod\
        &filter[from]=now-1h&filter[to]=now&page[limit]=5";
    let req = vetter_core::HttpRequest {
        method: vetter_core::HttpMethod::Get,
        url: url::Url::parse(long_url).unwrap(),
        headers: vec![],
        body: vetter_core::Body::None,
        auth: None,
        tls: vetter_core::TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    };
    let view = build_url_row(&req, false, mtm);
    let _ = &*view;
}

#[test]
fn build_url_row_smoke_handles_bare_host() {
    // `https://example.test` (empty path, no query) should hit the
    // single-row branch — the second (wrapping) row is skipped.
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let req = vetter_core::HttpRequest {
        method: vetter_core::HttpMethod::Get,
        url: url::Url::parse("https://example.test").unwrap(),
        headers: vec![],
        body: vetter_core::Body::None,
        auth: None,
        tls: vetter_core::TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    };
    let view = build_url_row(&req, false, mtm);
    let _ = &*view;
}
