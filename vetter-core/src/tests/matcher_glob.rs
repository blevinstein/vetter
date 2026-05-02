//! Tests for [`crate::matcher::glob`]. Layout convention is described
//! in `AGENTS.md`.

use super::*;

#[test]
fn host_exact_match_is_case_insensitive() {
    assert!(matches_host("api.github.com", "api.github.com"));
    assert!(matches_host("API.GITHUB.COM", "api.github.com"));
    assert!(!matches_host("api.github.com", "api.gitlab.com"));
}

#[test]
fn host_glob_matches_subdomains_only() {
    assert!(matches_host("*.github.com", "api.github.com"));
    assert!(matches_host("*.github.com", "a.b.github.com"));
    assert!(!matches_host("*.github.com", "github.com"));
}

#[test]
fn host_glob_rejects_suffix_confusion() {
    assert!(!matches_host("*.github.com", "github.com.evil.com"));
    assert!(!matches_host("*.github.com", "evilgithub.com"));
    assert!(!matches_host("github.com", "github.com.evil.com"));
    assert!(!matches_host("github.com", "evil.github.com"));
}

#[test]
fn host_invalid_glob_treated_as_literal() {
    assert!(!matches_host("*.", "anything"));
    assert!(!matches_host("github*.com", "githubx.com"));
    assert!(matches_host("github*.com", "github*.com"));
}

#[test]
fn path_exact() {
    assert!(matches_path("/repos", "/repos"));
    assert!(matches_path("repos", "/repos"));
    assert!(!matches_path("/repos", "/repos/foo"));
}

#[test]
fn path_single_star_matches_one_segment() {
    assert!(matches_path("/repos/*", "/repos/foo"));
    assert!(!matches_path("/repos/*", "/repos"));
    assert!(!matches_path("/repos/*", "/repos/foo/bar"));
}

#[test]
fn path_double_star_matches_any_segments() {
    assert!(matches_path("/repos/**", "/repos"));
    assert!(matches_path("/repos/**", "/repos/foo"));
    assert!(matches_path("/repos/**", "/repos/foo/bar/baz"));
    assert!(!matches_path("/repos/**", "/other"));
}

#[test]
fn path_double_star_in_middle() {
    assert!(matches_path("/a/**/c", "/a/c"));
    assert!(matches_path("/a/**/c", "/a/b/c"));
    assert!(matches_path("/a/**/c", "/a/b/x/c"));
    assert!(!matches_path("/a/**/c", "/a/b/x"));
}

#[test]
fn path_root() {
    assert!(matches_path("/", "/"));
    assert!(matches_path("/**", "/"));
    assert!(matches_path("/**", "/anything/here"));
}
