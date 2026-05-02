//! `vetter-core` — shared library for the `vetter` project.
//!
//! Module map:
//! - [`parsers`] — `CommandParser` trait, `ParsedCommand`, `Effect`, registry.
//! - [`render`]  — generic renderer over `ParsedCommand`.
//! - [`matcher`] — rule matcher (Phase 2; placeholder until then).
//! - [`signals`] — generic risk-signal analyzer.
//! - [`wire`]    — JSON wire types (Phase 3; placeholder until then).
//! - [`paths`]   — shared socket / pidfile resolution used by both
//!   `vet` and `vetterd`.

pub mod matcher;
pub mod parsers;
pub mod paths;
pub mod pidfile;
pub mod render;
pub mod signals;
pub mod wire;

pub use parsers::{
    register_builtins, Auth, Badge, BadgeSeverity, Body, CommandParser, CredentialUse,
    DisplayHints, Effect, EnvSnapshot, FileRead, FileWrite, FormField, Header, HttpMethod,
    HttpRequest, NetworkOpen, ParseError, ParsedCommand, ProcessSpawn, Sha256, StdinHandle,
    TlsPolicy, WriteSource,
};
pub use paths::{default_pidfile_path, default_socket_path};
pub use render::{AnsiWriter, DefaultRenderer, PlainWriter, Renderer, Style, StyledWriter};
pub use signals::{analyze, RiskSignal, SignalKind};
pub use wire::{
    new_request_id, read_decision, read_frame, read_request, write_frame, VetDecision, VetRequest,
    WireDecision, WireError, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};

/// Returns the crate version. Used by `vet doctor` for diagnostics.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;
