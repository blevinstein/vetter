//! Tests for [`crate::render::redact`]. Layout convention is described
//! in `AGENTS.md`.

use super::*;

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
