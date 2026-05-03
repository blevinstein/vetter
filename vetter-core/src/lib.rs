//! `vetter-core` — shared library for the `vetter` project.
//!
//! Module map:
//! - [`parsers`]     — `CommandParser` trait, `ParsedCommand`, `Effect`, registry.
//! - [`render`]      — generic renderer over `ParsedCommand`.
//! - [`matcher`]     — rule matcher (Phase 2; placeholder until then).
//! - [`signals`]     — generic risk-signal analyzer.
//! - [`known_hosts`] — known-hosts list loader and host-familiarity check.
//! - [`suggest`]     — generalisation engine (allowlist + known-host) for
//!   the macOS approver picker.
//! - [`wire`]        — JSON wire types (Phase 3; placeholder until then).
//! - [`paths`]       — shared socket / pidfile resolution used by both
//!   `vet` and `vetterd`.

pub mod known_hosts;
pub mod matcher;
pub mod parsers;
pub mod paths;
pub mod peer_cred;
pub mod pidfile;
pub mod render;
pub mod signals;
pub mod suggest;
pub mod wire;

pub use known_hosts::{load_default as load_known_hosts_default, KnownHostsStore};
pub use parsers::{
    register_builtins, Auth, Badge, BadgeSeverity, Body, CommandParser, CredentialUse,
    DisplayHints, Effect, EnvSnapshot, FileRead, FileWrite, FormField, Header, HttpMethod,
    HttpRequest, NetworkOpen, ParseError, ParsedCommand, ProcessSpawn, Sha256, StdinHandle,
    TlsPolicy, WriteSource,
};
pub use paths::{
    default_admin_socket_path, default_audit_path, default_pidfile_path, default_socket_path,
};
pub use render::{
    signal_kind_label, AnsiWriter, DefaultRenderer, PlainWriter, Renderer, Style, StyledWriter,
};
pub use signals::{analyze, check_known_hosts, RiskSignal, SignalKind};
pub use suggest::{
    allowlist_suggestions, host_suggestions, HostSuggestion, HostTier, RuleSuggestion,
    SuggestionTier,
};
pub use wire::{
    new_request_id, read_decision, read_frame, read_request, write_frame, MgmtRequest,
    MgmtResponse, PendingItem, VetDecision, VetRequest, WireDecision, WireError, WireScope,
    MAX_FRAME_BYTES, PROTOCOL_VERSION,
};

/// Returns the crate version. Used by `vet doctor` for diagnostics.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;
