//! Tests for [`crate::parsers`]. Layout convention is described in
//! `AGENTS.md`.

use super::*;

#[test]
fn basename_of_strips_directory() {
    assert_eq!(basename_of("/opt/homebrew/bin/curl"), "curl");
    assert_eq!(basename_of("curl"), "curl");
    assert_eq!(basename_of("./curl"), "curl");
    assert_eq!(basename_of(""), "");
}

#[test]
fn parse_error_messages_are_human_readable() {
    let e = ParseError::MissingArgument("URL".into()).to_string();
    assert!(e.contains("URL"), "{e}");
    let s = ParseError::StreamingUnsupported.to_string();
    assert!(s.contains("streaming"), "{s}");
}
