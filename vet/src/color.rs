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
#[path = "tests/color.rs"]
mod tests;
