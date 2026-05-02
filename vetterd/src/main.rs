//! `vetterd` binary entry point.
//!
//! Resolves env-driven paths and hands off to [`vetterd::run`]. Real
//! supervision (PID file, `vet daemon start|stop|status`) lands in
//! Phase 3b.

use std::path::PathBuf;
use std::process::ExitCode;

const EXIT_CONFIG: u8 = 78;

fn main() -> ExitCode {
    vetter_core::parsers::register_builtins();

    let socket_path = vetterd::default_socket_path();
    let audit_path = match vetterd::default_audit_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("vetterd: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    let allowlist_override = std::env::var_os("VETTER_ALLOWLIST").map(PathBuf::from);

    match vetterd::run(socket_path, audit_path, allowlist_override) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vetterd: {e}");
            ExitCode::from(EXIT_CONFIG)
        }
    }
}
