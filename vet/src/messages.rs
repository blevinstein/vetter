//! Shared human-readable message helpers for `vet`'s subcommands.
//!
//! Each handler module (`wrap`, `explain`, `allow`, ...) used to keep
//! its own copy of these formatters; that drifted whenever a new
//! variant was added (notably during the T4 argv0-resolver landing,
//! which had to update two identical `explain_resolve_error` bodies).
//! Centralising them here makes the next variant a one-place edit.
//!
//! The strings here are stable user-facing stderr text. Tests grep
//! against substrings of them; keep changes additive and search the
//! repo before retiring any phrase.

use vetter_core::matcher::LoadError;
use vetter_core::parsers::{ParseError, ResolveError};

/// Stable, human-readable summary of a [`ParseError`] for the CLI's
/// stderr output. Matches the variant names so users can grep them.
pub(crate) fn explain_parse_error(e: &ParseError) -> String {
    match e {
        ParseError::MissingArgument(what) => format!("missing required argument `{what}`"),
        ParseError::ConflictingArgs(detail) => format!("conflicting arguments: {detail}"),
        ParseError::UnknownArgument(name) => format!("unknown argument `{name}`"),
        ParseError::StreamingUnsupported => {
            "streaming bodies are not supported in this MVP (`-T -`, chunked transfer, \
             or `-d @-` over 1 MiB). See plans/Overview.md §8.4."
                .into()
        }
        ParseError::Other(s) => s.clone(),
    }
}

/// Human-friendly stderr line for argv0 resolution failures from
/// [`vetter_core::parsers::resolve_for_dispatch`]. Closes the
/// user-facing UX side of plans/ThreatModel.md T4: when the resolver
/// rejects, we tell the user *why* and how to opt back in via
/// `$VETTER_PARSER_TRUSTED_DIRS` for legitimate edge cases.
pub(crate) fn explain_resolve_error(e: &ResolveError) -> String {
    match e {
        ResolveError::EmptyArgv => "argv[0] is empty".into(),
        ResolveError::NotFound(name) => format!(
            "`{name}` was not found on $PATH or as a path on disk. \
             Check spelling, or pass an absolute path."
        ),
        ResolveError::NoParser(name) => format!(
            "no parser registered for `{name}`. Phase 1b ships with `curl` only; \
             see plans/Overview.md §11 for the parser roadmap."
        ),
        ResolveError::UntrustedBinary {
            argv0,
            resolved,
            parser_name,
        } => format!(
            "`{argv0}` resolves to `{}`, which is not the canonical `{parser_name}` \
             on $PATH and is not in a trusted install dir. This blocks the argv0 \
             spoofing path described in plans/ThreatModel.md T4 \
             (e.g. `ln /bin/bash /tmp/curl`). If your `{parser_name}` lives in an \
             unusual location, add it to $VETTER_PARSER_TRUSTED_DIRS \
             (`:`-separated, like $PATH).",
            resolved.display()
        ),
        ResolveError::Io { path, source } => {
            format!("io error resolving `{}`: {source}", path.display())
        }
    }
}

/// Stable, human-readable summary of a [`LoadError`] for the CLI's
/// stderr output. Surfaces both the failing path and the underlying
/// io / yaml diagnostic so users can locate the broken rule quickly.
pub(crate) fn explain_load_error(e: &LoadError) -> String {
    match e {
        LoadError::Io { path, source } => format!("read {}: {source}", path.display()),
        LoadError::Yaml { path, source } => format!("parse {}: {source}", path.display()),
        LoadError::Serialize { path, source } => {
            format!("serialise {}: {source}", path.display())
        }
        LoadError::DuplicateId { id, path } => {
            format!("duplicate rule id `{id}` in {}", path.display())
        }
        LoadError::RuleNotFound { id, path } => {
            format!("no rule with id `{id}` in {}", path.display())
        }
    }
}
