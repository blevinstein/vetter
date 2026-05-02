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
