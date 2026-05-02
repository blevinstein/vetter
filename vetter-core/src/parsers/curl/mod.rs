//! Curl `CommandParser`. See `plans/Overview.md` §8.4.
//!
//! Layout:
//! - [`flags`]: pure data (flag table + tokeniser).
//! - [`state`]: the collector + finaliser that turns tokens into a
//!   [`ParsedCommand`].
//!
//! The implementation is intentionally fail-closed: any invocation we
//! cannot represent precisely returns a [`ParseError`] so `vet` refuses
//! to run rather than mis-vetting it.

pub mod flags;
mod state;

use super::{CommandParser, EnvSnapshot, ParseError, ParsedCommand, StdinHandle};

pub struct CurlParser;

impl CommandParser for CurlParser {
    fn name(&self) -> &'static str {
        "curl"
    }

    fn handles(&self, argv0: &str) -> bool {
        // Basename matching is done by the registry; we just need to
        // recognise the canonical name. Future variants like
        // `curl-impersonate` can be added here without table changes.
        argv0 == "curl"
    }

    fn parse(
        &self,
        argv: &[String],
        stdin: StdinHandle<'_>,
        _env: &EnvSnapshot,
    ) -> Result<ParsedCommand, ParseError> {
        state::parse_argv(argv, &stdin)
    }
}
