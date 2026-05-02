//! `vetter-core` — shared library for the `vetter` project.
//!
//! Phase 0 only sets up the module structure that subsequent phases will
//! flesh out:
//!
//! - [`parsers`] — `CommandParser` trait, `ParsedCommand`, `Effect`, registry (Phase 1a).
//! - [`render`]  — generic renderer over `ParsedCommand` (Phase 1a).
//! - [`matcher`] — rule matcher (Phase 2).
//! - [`signals`] — generic risk-signal analyzer (Phase 1a / Phase 2).
//! - [`wire`]    — JSON wire types for the `vet` ⇄ `vetterd` socket (Phase 3).

pub mod matcher;
pub mod parsers;
pub mod render;
pub mod signals;
pub mod wire;

/// Returns the crate version. Used by `vet doctor` for diagnostics.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
