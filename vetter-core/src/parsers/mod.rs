//! Parser plugin layer.
//!
//! Every command-aware parser implements [`CommandParser`] and is
//! registered into the static registry by the binary (or by tests, for
//! the noop parser). All downstream consumers — renderer, analyzer,
//! future matcher — read [`ParsedCommand`] instances; they never branch
//! on the command name.

use std::collections::HashMap;
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

mod types;

pub use types::*;

pub mod curl;

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

// --- argv0 inode resolution (ThreatModel.md T4) ---------------------------

/// Result of [`resolve_for_dispatch`]: a parser handle plus the canonical
/// on-disk path the dispatcher resolved `argv[0]` to.
///
/// Callers should `exec` [`Self::resolved_path`] (with
/// `arg0(&original_argv0)` to preserve the program-visible `argv[0]`)
/// instead of the original argv[0]. That binds vetting and execution to
/// the same `(dev, ino)` so a same-UID racer cannot swap a different
/// binary into place between the parse and the exec.
#[derive(Debug)]
pub struct ResolvedCommand {
    pub parser: RegisteredParser,
    pub resolved_path: PathBuf,
}

/// Errors from [`resolve_for_dispatch`].
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("argv[0] is empty")]
    EmptyArgv,
    #[error("`{0}` not found on PATH or as a path on disk")]
    NotFound(String),
    #[error("no parser registered for `{0}` (basename of resolved path)")]
    NoParser(String),
    #[error(
        "`{argv0}` resolves to `{resolved}`, which is neither the canonical `{parser_name}` on PATH nor inside a trusted install dir (override via $VETTER_PARSER_TRUSTED_DIRS)"
    )]
    UntrustedBinary {
        argv0: String,
        resolved: PathBuf,
        parser_name: &'static str,
    },
    #[error("io error resolving `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Hybrid argv0 resolver. Closes ThreatModel.md T4 (argv0 spoofing).
///
/// Resolves `argv[0]` to a canonical filesystem path and accepts it iff
/// **either**:
/// - (a) its `(dev, ino)` matches the canonical `<parser_name>` binary
///   discovered on `$PATH`, **or**
/// - (b) its parent directory is inside the trusted-install-dir set
///   (`/usr/bin`, `/usr/local/bin`, `/opt/homebrew/bin`, `/opt/local/bin`,
///   plus `$VETTER_PARSER_TRUSTED_DIRS` colon-separated extension).
///
/// This catches hardlinks (which `canonicalize` does not follow),
/// symlinks to other binaries (which `canonicalize` *does* follow,
/// changing the basename and thereby the parser dispatch), and direct
/// copies of arbitrary binaries (different inode from the canonical
/// one on PATH, and typically not in a trusted install dir).
pub fn resolve_for_dispatch(argv0: &str) -> Result<ResolvedCommand, ResolveError> {
    resolve_with_env(argv0, &ResolveEnv::from_process())
}

/// Snapshot of process env relevant to argv0 resolution. Carved out as
/// a separate type so unit tests can inject `PATH` /
/// `VETTER_PARSER_TRUSTED_DIRS` without mutating the process-global env
/// (which would race with parallel tests).
#[derive(Debug, Default, Clone)]
struct ResolveEnv {
    path: Option<std::ffi::OsString>,
    extra_trusted: Option<std::ffi::OsString>,
}

impl ResolveEnv {
    fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            extra_trusted: std::env::var_os("VETTER_PARSER_TRUSTED_DIRS"),
        }
    }
}

const BUILTIN_TRUSTED_DIRS: &[&str] = &[
    "/usr/bin",
    "/usr/local/bin",
    "/opt/homebrew/bin",
    "/opt/local/bin",
];

fn resolve_with_env(argv0: &str, env: &ResolveEnv) -> Result<ResolvedCommand, ResolveError> {
    if argv0.is_empty() {
        return Err(ResolveError::EmptyArgv);
    }

    // 1. Locate the named binary on disk. argv0 with a `/` is a path;
    //    otherwise walk PATH.
    let located: PathBuf = if argv0.contains('/') {
        PathBuf::from(argv0)
    } else {
        which_in_path(argv0, env.path.as_deref())
            .ok_or_else(|| ResolveError::NotFound(argv0.to_string()))?
    };

    // 2. Canonicalise (follows symlinks; hardlinks share the original's
    //    inode and remain caught by the (dev, ino) check below).
    let resolved = std::fs::canonicalize(&located).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ResolveError::NotFound(argv0.to_string())
        } else {
            ResolveError::Io {
                path: located.clone(),
                source,
            }
        }
    })?;

    // 3. Look up the parser by basename of the canonical path. A
    //    `/tmp/curl` symlink to `/bin/bash` canonicalises to `/bin/bash`
    //    and dispatch fails here (no parser for "bash") — that's the
    //    cheapest catch and it fires before either trust check.
    let basename = resolved.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let parser = dispatch(basename).ok_or_else(|| ResolveError::NoParser(basename.to_string()))?;
    let parser_name = parser.name();

    // 4a. Inode match against `which <parser_name>` on PATH.
    if let Some(canonical_on_path) = which_in_path(parser_name, env.path.as_deref()) {
        if let (Ok(want), Ok(got)) = (file_id(&canonical_on_path), file_id(&resolved)) {
            if want == got {
                return Ok(ResolvedCommand {
                    parser,
                    resolved_path: resolved,
                });
            }
        }
    }

    // 4b. Canonical path's parent in the trusted-install-dir set.
    if let Some(parent) = resolved.parent() {
        let canon_dirs = trusted_install_dirs(env.extra_trusted.as_deref());
        if canon_dirs.iter().any(|d| d == parent) {
            return Ok(ResolvedCommand {
                parser,
                resolved_path: resolved,
            });
        }
    }

    Err(ResolveError::UntrustedBinary {
        argv0: argv0.to_string(),
        resolved,
        parser_name,
    })
}

/// Walk a colon-separated PATH for an executable file matching `name`.
/// Returns the canonicalised hit (so symlinks in PATH dirs collapse).
fn which_in_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let path = path?;
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(std::fs::canonicalize(&candidate).unwrap_or(candidate));
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn file_id(path: &Path) -> std::io::Result<(u64, u64)> {
    let m = std::fs::metadata(path)?;
    Ok((m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_path: &Path) -> std::io::Result<(u64, u64)> {
    // Non-unix has no `(dev, ino)` equivalent, so the inode-match arm
    // never fires; callers fall through to the path-list check.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "file_id requires unix",
    ))
}

/// Returns the canonicalised set of directories considered "trusted
/// install dirs" for `resolve_for_dispatch`. Nonexistent dirs are
/// silently dropped — common on systems without Homebrew or MacPorts.
fn trusted_install_dirs(extra: Option<&OsStr>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = BUILTIN_TRUSTED_DIRS.iter().map(PathBuf::from).collect();
    if let Some(extra) = extra {
        for d in std::env::split_paths(extra) {
            if d.as_os_str().is_empty() {
                continue;
            }
            out.push(d);
        }
    }
    out.into_iter()
        .filter_map(|p| std::fs::canonicalize(&p).ok())
        .collect()
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

/// Register every parser that ships in release `vet` / `vetterd`
/// binaries. Idempotent: callable from multiple binary entry points
/// (and tests) without panicking on duplicate registration.
///
/// Intentionally does NOT register the `noop` parser — that one is
/// gated behind `cfg(any(test, feature = "test-parsers"))` and stays
/// out of release builds.
pub fn register_builtins() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        register(Box::new(curl::CurlParser));
    });
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
#[path = "../tests/parsers.rs"]
mod tests;
