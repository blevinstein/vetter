//! `vet doctor` — diagnostic stub.
//!
//! Per `plans/Overview.md` §4, doctor checks daemon, socket, parsers, and
//! code-signing status. Phase 0 reports each as "not implemented" with
//! the roadmap phase that will fill it in. Exits 0 — no checks fail
//! because none exist yet.

use std::process::ExitCode;

pub fn run() -> ExitCode {
    println!("vet doctor (vetter-core {})", vetter_core::version());
    println!("  daemon ............. not implemented (Phase 3; see plans/Overview.md §11)");
    println!("  socket ............. not implemented (Phase 3; see plans/Overview.md §11)");
    println!(
        "  parsers registered . {}  (Phase 1b will register the curl parser)",
        vetter_core::parsers::registered_count()
    );
    println!("  code signing ....... not implemented (Phase 4; see plans/Overview.md §11)");
    ExitCode::SUCCESS
}
