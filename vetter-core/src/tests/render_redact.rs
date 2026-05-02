//! Tests for [`crate::render::redact`]. Layout convention is described
//! in `AGENTS.md`.

use super::*;

#[test]
fn known_secret_headers_are_recognised() {
    for h in [
        "Authorization",
        "AUTHORIZATION",
        "authorization",
        "Cookie",
        "X-Api-Key",
        "x-api-key",
        "Proxy-Authorization",
        "X-Vault-Token",
        "X-Github-Token",
    ] {
        assert!(is_secret_header(h), "{h} should be secret");
    }
}

#[test]
fn benign_headers_are_not_redacted() {
    for h in [
        "Content-Type",
        "Accept",
        "User-Agent",
        "X-Request-Id",
        "X-Correlation-Id",
        "X-Token-Issued-At",
    ] {
        assert!(!is_secret_header(h), "{h} should NOT be secret");
    }
}

#[test]
fn redact_value_keeps_last_four_chars() {
    assert_eq!(redact_value("Bearer abcdef1234"), "••••1234");
}

#[test]
fn redact_value_for_short_string() {
    assert_eq!(redact_value("abc"), "••••");
    assert_eq!(redact_value(""), "••••");
}

#[test]
fn redact_value_does_not_split_multibyte() {
    let v = "héllo世界";
    let out = redact_value(v);
    assert!(out.starts_with("••••"));
    assert!(out.ends_with("lo世界"), "got `{out}`");
}
