//! Tests for [`crate::matcher::url`]. Layout convention is described
//! in `AGENTS.md`.

use super::*;

fn url(s: &str) -> Url {
    Url::parse(s).expect("test url")
}

#[test]
fn dot_segments_resolved() {
    assert_eq!(normalise_path("/a/./b"), "/a/b");
    assert_eq!(normalise_path("/a/./"), "/a/");
    assert_eq!(normalise_path("/./"), "/");
}

#[test]
fn double_dot_pops() {
    assert_eq!(normalise_path("/admin/../secret"), "/secret");
    assert_eq!(normalise_path("/a/b/../c"), "/a/c");
    assert_eq!(normalise_path("/a/b/../../c"), "/c");
}

#[test]
fn double_dot_above_root_clamps() {
    assert_eq!(normalise_path("/../secret"), "/secret");
    assert_eq!(normalise_path("/../../"), "/");
    assert_eq!(normalise_path("/a/../../b"), "/b");
}

#[test]
fn root_and_empty() {
    assert_eq!(normalise_path("/"), "/");
    assert_eq!(normalise_path(""), "/");
}

#[test]
fn trailing_slash_preserved_when_meaningful() {
    assert_eq!(normalise_path("/a/b/"), "/a/b/");
    assert_eq!(normalise_path("/a/b"), "/a/b");
    assert_eq!(normalise_path("/a/b/./"), "/a/b/");
}

#[test]
fn url_normalise_drops_dotdot() {
    let u = url("https://example.test/admin/../secret?q=1");
    let n = normalise(&u);
    assert_eq!(n.path(), "/secret");
    assert_eq!(n.query(), Some("q=1"));
}

#[test]
fn url_normalise_does_not_lowercase_path() {
    let u = url("https://example.test/Admin/Foo");
    let n = normalise(&u);
    assert_eq!(n.path(), "/Admin/Foo");
}
