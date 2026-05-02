//! Integration tests for the parser registry (TestingPlan §2.2).
//!
//! All assertions live in a single `#[test]` function because the
//! registry is a process-global; running multiple `#[test]`s in
//! parallel against it would race on `register()`. The
//! `registry_duplicate.rs` sibling exercises the panic-on-duplicate
//! path in its own process.

use vetter_core::parsers::{self, noop::NoopParser, EnvSnapshot, StdinHandle};

#[test]
fn registry_dispatch_and_parse() {
    // Empty registry: unknown lookup is None.
    assert!(parsers::dispatch("definitely-not-a-parser").is_none());
    assert_eq!(parsers::registered_count(), 0);

    parsers::register(Box::new(NoopParser));

    assert_eq!(parsers::registered_count(), 1);
    assert!(parsers::registered_names().contains(&"noop"));

    // Bare name.
    let by_name = parsers::dispatch("noop").expect("plain name");
    assert_eq!(by_name.name(), "noop");

    // Absolute and relative paths fall back to basename.
    assert_eq!(
        parsers::dispatch("/usr/local/bin/noop")
            .expect("absolute basename")
            .name(),
        "noop"
    );
    assert_eq!(
        parsers::dispatch("./noop")
            .expect("relative basename")
            .name(),
        "noop"
    );

    // Unknown still misses.
    assert!(parsers::dispatch("nope").is_none());

    // Dispatch round-trip through parse.
    let p = parsers::dispatch("noop")
        .unwrap()
        .parse(
            &["noop".to_string(), "https://example.test/x".to_string()],
            StdinHandle::empty(),
            &EnvSnapshot::default(),
        )
        .expect("parse");
    assert_eq!(p.command, "noop");
    assert_eq!(p.effects.len(), 1);
}
