//! End-to-end pipeline test: register noop → dispatch → parse → analyze
//! → render. Uses `PlainWriter` so the assertion is ANSI-free.

use vetter_core::{
    analyze,
    parsers::{self, noop::NoopParser, EnvSnapshot, StdinHandle},
    DefaultRenderer, PlainWriter, Renderer,
};

#[test]
fn full_pipeline_renders_url() {
    parsers::register(Box::new(NoopParser));
    let dispatch = parsers::dispatch("noop").expect("registered");
    let mut parsed = dispatch
        .parse(
            &["noop".to_string(), "https://example.test/path".to_string()],
            StdinHandle::empty(),
            &EnvSnapshot::default(),
        )
        .expect("parse");
    parsed.signals = analyze(&parsed);

    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(&parsed, None, &mut PlainWriter(&mut buf))
        .expect("render");
    let out = String::from_utf8(buf).expect("utf8");

    assert!(out.contains("noop"), "expected `noop` in {out}");
    assert!(
        out.contains("https://example.test/path"),
        "url missing in {out}"
    );
    assert!(out.contains("Match:"), "match line missing in {out}");
}
