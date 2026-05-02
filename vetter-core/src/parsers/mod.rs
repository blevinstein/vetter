//! Parser plugin layer.
//!
//! Every command-aware parser implements [`CommandParser`] and is
//! registered into the static registry by the binary (or by tests, for
//! the noop parser). All downstream consumers — renderer, analyzer,
//! future matcher — read [`ParsedCommand`] instances; they never branch
//! on the command name.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

mod types;

pub use types::*;

#[cfg(any(test, feature = "test-parsers"))]
pub mod noop;

/// The plugin contract from `plans/Overview.md` §8.1.
pub trait CommandParser: Send + Sync {
    /// Stable identifier ("curl", "wget", "noop", ...). Appears in rules,
    /// audit logs, and the wire protocol.
    fn name(&self) -> &'static str;

    /// True if this parser handles the given `argv[0]`. The registry
    /// also supplies the basename (filename without directory) so a
    /// parser only needs to match `"curl"` to handle
    /// `/opt/homebrew/bin/curl`.
    fn handles(&self, argv0: &str) -> bool;

    /// Parse argv into the shared output model. Returning `Err` means
    /// the invocation is unparseable; `vet` then refuses to run it
    /// rather than guessing — refusing is safer than mis-vetting.
    fn parse(
        &self,
        argv: &[String],
        stdin: StdinHandle<'_>,
        env: &EnvSnapshot,
    ) -> Result<ParsedCommand, ParseError>;
}

/// Errors a parser may return. The set is intentionally small: any case
/// the parser cannot represent precisely should surface as an error so
/// `vet` fails closed instead of silently mis-vetting.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("missing required argument: {0}")]
    MissingArgument(String),
    #[error("conflicting arguments: {0}")]
    ConflictingArgs(String),
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    #[error(
        "streaming bodies are not supported in MVP (`-T -`, `-d @-` over 1 MiB, or chunked transfer)"
    )]
    StreamingUnsupported,
    #[error("{0}")]
    Other(String),
}

/// A bounded handle to the wrapped command's stdin. Phase 1a never reads
/// from this; Phase 1b's curl parser uses it for `-d @-` bodies and
/// hashes the contents.
#[derive(Debug, Default)]
pub struct StdinHandle<'a> {
    /// `Some` if the caller pre-read stdin (test path or a non-pipe).
    /// `None` means the caller passed no stdin.
    pub buf: Option<&'a [u8]>,
    /// Maximum bytes a parser may consume. Reading past this returns
    /// [`ParseError::StreamingUnsupported`] in Phase 1b.
    pub cap_bytes: usize,
}

impl<'a> StdinHandle<'a> {
    pub fn empty() -> Self {
        Self {
            buf: None,
            cap_bytes: 0,
        }
    }

    pub fn from_bytes(buf: &'a [u8], cap_bytes: usize) -> Self {
        Self {
            buf: Some(buf),
            cap_bytes,
        }
    }
}

/// Read-only snapshot of the environment a parser may inspect (e.g.
/// `CURL_HOME`, `AWS_PROFILE`). Also carries `cwd` since some parsers
/// resolve relative paths against it.
#[derive(Debug, Clone, Default)]
pub struct EnvSnapshot {
    pub vars: HashMap<String, String>,
    pub cwd: Option<std::path::PathBuf>,
}

impl EnvSnapshot {
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
            cwd: std::env::current_dir().ok(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }
}

// --- registry -------------------------------------------------------------

type Registry = Mutex<Vec<Box<dyn CommandParser>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a parser. Panics if a parser with the same `name()` is
/// already registered — that's a programming error caught early.
pub fn register(parser: Box<dyn CommandParser>) {
    let mut g = registry().lock().expect("parser registry poisoned");
    if g.iter().any(|p| p.name() == parser.name()) {
        panic!(
            "duplicate parser registration: `{}` is already registered",
            parser.name()
        );
    }
    g.push(parser);
}

/// Look up a parser by `argv[0]`. Tries the full path first, then the
/// basename, so `/opt/homebrew/bin/curl` resolves to the curl parser.
pub fn dispatch(argv0: &str) -> Option<RegisteredParser> {
    let g = registry().lock().expect("parser registry poisoned");
    let basename = basename_of(argv0);
    let idx = g
        .iter()
        .position(|p| p.handles(argv0) || p.handles(basename))?;
    Some(RegisteredParser { idx })
}

fn basename_of(p: &str) -> &str {
    Path::new(p)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(p)
}

/// Returns the number of registered parsers. Used by `vet doctor`.
pub fn registered_count() -> usize {
    registry().lock().expect("parser registry poisoned").len()
}

/// Names of currently-registered parsers, in registration order. Used
/// by `vet doctor` to show what's loaded.
pub fn registered_names() -> Vec<&'static str> {
    registry()
        .lock()
        .expect("parser registry poisoned")
        .iter()
        .map(|p| p.name())
        .collect()
}

/// Stable handle to a registered parser. We can't return `&dyn` because
/// the registry is behind a mutex; this handle re-locks per call. That's
/// fine for the dispatch hot path (one call per `vet` invocation).
#[derive(Debug, Clone, Copy)]
pub struct RegisteredParser {
    idx: usize,
}

impl RegisteredParser {
    pub fn name(&self) -> &'static str {
        let g = registry().lock().expect("parser registry poisoned");
        g[self.idx].name()
    }

    pub fn parse(
        &self,
        argv: &[String],
        stdin: StdinHandle<'_>,
        env: &EnvSnapshot,
    ) -> Result<ParsedCommand, ParseError> {
        let g = registry().lock().expect("parser registry poisoned");
        g[self.idx].parse(argv, stdin, env)
    }
}

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use super::*;

    #[test]
    fn basename_of_strips_directory() {
        assert_eq!(basename_of("/opt/homebrew/bin/curl"), "curl");
        assert_eq!(basename_of("curl"), "curl");
        assert_eq!(basename_of("./curl"), "curl");
        assert_eq!(basename_of(""), "");
    }

    #[test]
    fn parse_error_messages_are_human_readable() {
        let e = ParseError::MissingArgument("URL".into()).to_string();
        assert!(e.contains("URL"), "{e}");
        let s = ParseError::StreamingUnsupported.to_string();
        assert!(s.contains("streaming"), "{s}");
    }
}
