//! Output styling decision per `plans/Overview.md` §4.
//!
//! Resolves the well-known environment + TTY signals into a single
//! [`Style`] discriminant. The renderer doesn't care about envs; only
//! the binary entry point makes this choice.
//!
//! Precedence (matches `clicolors` and Rust's `anstyle-query`):
//!
//! 1. `NO_COLOR` set to a non-empty value → [`Style::Plain`].
//! 2. `CLICOLOR_FORCE` set, non-empty, and not `"0"` → [`Style::Ansi`].
//! 3. Otherwise: ANSI iff the target stream is a terminal.

use std::io::IsTerminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Ansi,
}

/// Pick a style for the given stream using the live process env. The
/// `IsTerminal` bound is sealed in std (so we can't fake it in tests);
/// for testing the env precedence we use [`pick_inner`] directly.
pub fn pick<W: IsTerminal>(stream: &W) -> Style {
    pick_inner(stream.is_terminal(), |k| std::env::var_os(k))
}

/// Test-friendly core: takes a boolean `is_terminal` and an env lookup
/// closure. Avoids the sealed `IsTerminal` trait so we can exercise
/// every NO_COLOR / CLICOLOR_FORCE path deterministically.
fn pick_inner<F>(is_terminal: bool, env: F) -> Style
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return Style::Plain;
    }
    if env("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty() && v != "0") {
        return Style::Ansi;
    }
    if is_terminal {
        Style::Ansi
    } else {
        Style::Plain
    }
}

#[cfg(test)]
mod tests {
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
}
