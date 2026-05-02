//! Default path resolution for the daemon.
//!
//! All three resolvers (socket, pidfile, audit log) and the
//! `PathError` type live in [`vetter_core::paths`] so `vet`,
//! `vetterd`, and `vet doctor` agree byte-for-byte. This module just
//! re-exports them so existing call sites (`vetterd::paths::…`) keep
//! working.

pub use vetter_core::paths::{
    default_admin_socket_path, default_audit_path, default_pidfile_path, default_socket_path,
    PathError,
};

#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
#[path = "tests/paths.rs"]
mod tests;
