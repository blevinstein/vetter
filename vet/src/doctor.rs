//! `vet doctor` — diagnostic stub.
//!
//! Per `plans/Overview.md` §4, doctor checks daemon, socket, parsers, and
//! code-signing status. Phase 1b reports the registered parser names;
//! daemon, socket, and code-signing remain "not implemented" with the
//! roadmap phase that will fill them in. Exits 0 — no checks fail.

use std::process::ExitCode;

pub fn run() -> ExitCode {
    let names = vetter_core::parsers::registered_names();
    let summary = if names.is_empty() {
        "none yet".to_string()
    } else {
        names.join(", ")
    };

    println!("vet doctor (vetter-core {})", vetter_core::version());
    println!("  daemon ............. not implemented (Phase 3; see plans/Overview.md §11)");
    println!("  socket ............. not implemented (Phase 3; see plans/Overview.md §11)");
    println!("  parsers registered . {}  ({summary})", names.len());
    println!("  code signing ....... not implemented (Phase 4; see plans/Overview.md §11)");
    ExitCode::SUCCESS
}
