//! Tests for [`crate`] root. Layout convention is described in
//! `AGENTS.md`.

use super::*;

#[test]
fn version_is_non_empty() {
    assert!(!version().is_empty());
}
