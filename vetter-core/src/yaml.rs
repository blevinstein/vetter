//! The single entry point for YAML (de)serialisation.
//!
//! Every vetter-owned YAML file — allowlists, known-hosts, settings —
//! and every rendered rule preview goes through here, so the parser
//! options below apply uniformly. Project-scoped files are read from
//! whatever repository the user is working in, which makes this an
//! untrusted-input parser (see `plans/ThreatModel.md`).

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_saphyr::{DuplicateKeyPolicy, Options};

pub use serde_saphyr::SerializeError as SerError;

/// Boxed because the parser's error carries a source snippet and
/// location, which would otherwise bloat every `Result` that wraps it.
pub type DeError = Box<serde_saphyr::Error>;

/// Parse `input` as a single YAML document.
///
/// Duplicate keys are an error, so a second `path:` cannot silently
/// override the one a reviewer read. Booleans accept only `true` /
/// `false`; YAML 1.1 forms like `yes` / `on` are rejected rather than
/// coerced. The library's default alias budget stays in force.
/// Errors are a single line (with line and column) so they fit the
/// `vet doctor` table and one-line CLI diagnostics.
pub fn from_str<T: DeserializeOwned>(input: &str) -> Result<T, DeError> {
    let mut options = Options::default();
    options.duplicate_keys = DuplicateKeyPolicy::Error;
    options.strict_booleans = true;
    options.with_snippet = false;
    serde_saphyr::from_str_with_options(input, options).map_err(Box::new)
}

/// Serialise `value` as a YAML document.
pub fn to_string<T: Serialize>(value: &T) -> Result<String, SerError> {
    serde_saphyr::to_string(value)
}

#[cfg(test)]
#[path = "tests/yaml.rs"]
mod tests;
