//! Unit tests for [`crate::color`]. Layout convention is
//! described in `AGENTS.md`.

use super::*;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;

fn env_lookup(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<OsString> {
    let owned: HashMap<String, OsString> = map
        .into_iter()
        .map(|(k, v)| (k.to_string(), OsString::from(v)))
        .collect();
    move |k: &str| owned.get(k).cloned()
}

#[test]
fn plain_when_stream_not_a_tty_and_no_env() {
    assert_eq!(pick_inner(false, env_lookup(HashMap::new())), Style::Plain);
}

#[test]
fn ansi_when_stream_is_a_tty_and_no_env() {
    assert_eq!(pick_inner(true, env_lookup(HashMap::new())), Style::Ansi);
}

#[test]
fn no_color_overrides_tty() {
    let env = HashMap::from([("NO_COLOR", "1")]);
    assert_eq!(pick_inner(true, env_lookup(env)), Style::Plain);
}

#[test]
fn empty_no_color_does_not_force_plain() {
    let env = HashMap::from([("NO_COLOR", "")]);
    assert_eq!(pick_inner(true, env_lookup(env.clone())), Style::Ansi);
    assert_eq!(pick_inner(false, env_lookup(env)), Style::Plain);
}

#[test]
fn clicolor_force_overrides_non_tty() {
    let env = HashMap::from([("CLICOLOR_FORCE", "1")]);
    assert_eq!(pick_inner(false, env_lookup(env)), Style::Ansi);
}

#[test]
fn clicolor_force_zero_is_a_no_op() {
    let env = HashMap::from([("CLICOLOR_FORCE", "0")]);
    assert_eq!(pick_inner(false, env_lookup(env)), Style::Plain);
}

#[test]
fn no_color_beats_clicolor_force() {
    let env = HashMap::from([("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")]);
    assert_eq!(pick_inner(true, env_lookup(env.clone())), Style::Plain);
    assert_eq!(pick_inner(false, env_lookup(env)), Style::Plain);
}

#[test]
fn pick_against_real_stderr_does_not_panic() {
    // `io::stderr()` implements `IsTerminal`; this is a thin wiring
    // smoke check — the result depends on the test runner's stdio.
    let s = pick(&io::stderr());
    assert!(matches!(s, Style::Plain | Style::Ansi));
}
