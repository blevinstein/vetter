//! Insta snapshot of the noop parser's rendered output (TestingPlan §2.3).
//! Captures both plain (ANSI-stripped) and styled forms so a regression
//! in either is caught.

use vetter_core::{
    analyze,
    parsers::{noop::NoopParser, CommandParser, EnvSnapshot, StdinHandle},
    AnsiWriter, DefaultRenderer, PlainWriter, Renderer,
};

fn noop_parsed() -> vetter_core::ParsedCommand {
    let parser = NoopParser;
    let mut p = parser
        .parse(
            &["noop".to_string()],
            StdinHandle::empty(),
            &EnvSnapshot::default(),
        )
        .expect("parse");
    p.signals = analyze(&p);
    p
}

#[test]
fn noop_render_plain() {
    let p = noop_parsed();
    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(&p, &mut PlainWriter(&mut buf))
        .unwrap();
    let out = String::from_utf8(buf).unwrap();
    insta::assert_snapshot!("noop_plain", out);
}

#[test]
fn noop_render_ansi() {
    let p = noop_parsed();
    let mut buf = Vec::<u8>::new();
    DefaultRenderer
        .render(&p, &mut AnsiWriter(&mut buf))
        .unwrap();
    let out = String::from_utf8(buf).unwrap();
    // Replace ESC with a printable marker so the snapshot reads cleanly
    // and diffs are stable across terminals.
    let pretty = out.replace('\x1b', "\\e");
    insta::assert_snapshot!("noop_ansi", pretty);
}
