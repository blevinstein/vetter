//! `vetterd` — long-running per-user daemon for vetter.
//!
//! Phase 0 stub. The real socket listener, request queue, and audit log
//! land in Phase 3 (`plans/Overview.md` §3, §11).

use std::process::ExitCode;

fn main() -> ExitCode {
    println!(
        "vetterd Phase 0 stub (vetter-core {}). Socket listener lands in Phase 3.",
        vetter_core::version()
    );
    ExitCode::SUCCESS
}
