//! Parser plugin layer.
//!
//! Phase 1a will populate this module with the `CommandParser` trait,
//! `ParsedCommand`, `Effect`, and the static registry described in
//! `plans/Overview.md` §8. For Phase 0 we only expose a placeholder
//! count so `vet doctor` can report `parsers registered . 0`.

/// Number of parsers currently registered. Always 0 in Phase 0.
pub fn registered_count() -> usize {
    0
}
