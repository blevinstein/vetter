//! Tests for [`crate::runloop::popover_url`].
//!
//! Only the AppKit half lives here now. Host-trust classification
//! and URL segmentation moved into the shared card layer along with
//! their assertions — see `crate::tests::cards_url`, which runs on
//! every target.
//!
//! What remains are bare smoke tests that check we can construct the
//! views without panicking on the main thread; the per-token visual
//! styling is verified manually in the Phase 4 smoke runs.

#![cfg(target_os = "macos")]

use super::*;

fn get_request(url: &str) -> vetter_core::HttpRequest {
    vetter_core::HttpRequest {
        method: vetter_core::HttpMethod::Get,
        url: url::Url::parse(url).unwrap(),
        headers: vec![],
        body: vetter_core::Body::None,
        auth: None,
        tls: vetter_core::TlsPolicy::Strict,
        follow_redirects: false,
        proxy: None,
    }
}

#[test]
fn build_url_row_smoke_runs_without_panicking() {
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return;
    };
    let req = get_request("https://api.example.com:8443/v1/things?q=1");
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
    let req = get_request(long_url);
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
    let req = get_request("https://example.test");
    let view = build_url_row(&req, false, mtm);
    let _ = &*view;
}
