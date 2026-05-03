//! Rule matcher: evaluates a [`crate::ParsedCommand`] against the layered
//! allowlist + denylist defined in `plans/Overview.md` §5.
//!
//! Layout:
//! - [`rule`]    — `Rule`, `RuleWhen`, clause types + `serde` derive.
//! - [`glob`]    — host + path globs (no regex dep).
//! - [`url`]     — URL canonicalisation (resolves `.` / `..`).
//! - [`decide`]  — `Decision`, `Scope`, `decide()` entry point.
//! - [`loader`]  — YAML schema, `AllowlistStore`, project discovery.
//!
//! Public surface intentionally narrow: callers grab an
//! [`AllowlistStore`] from [`load_default`] and feed each
//! [`crate::ParsedCommand`] through [`decide`] to get a [`Decision`].

pub mod decide;
pub mod glob;
pub mod loader;
pub mod rule;
pub mod url;

pub use decide::{decide, matches_rule, Decision, Scope};
pub use loader::{
    add_rule, discover_project_root, load_default, load_file, remove_rule, user_allowlist_path,
    write_file, AllowlistFile, AllowlistStore, LoadError,
};
pub use rule::{
    FileReadClause, FileWriteClause, HostPattern, HttpClause, Rule, RuleWhen, UrlClause,
};

use sha2::{Digest, Sha256};

/// Deterministic id for a rule whose author did not supply one.
///
/// Computes `auto-<8-hex>` over the canonical YAML serialisation of
/// `rule` (with the `id` field cleared first so id-derivation is
/// idempotent regardless of any preset id). Used by both `vet allow
/// add` and the daemon's pattern-suggestion engine so re-clicking
/// the same suggestion produces the same id (the loader's
/// duplicate-id check then short-circuits a second persist).
pub fn derive_auto_id(rule: &Rule) -> String {
    let mut canon = rule.clone();
    canon.id = String::new();
    let yaml = serde_yaml_ng::to_string(&canon).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(yaml.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("auto-{hex}")
}
