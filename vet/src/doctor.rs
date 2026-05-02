//! `vet doctor` — diagnostic stub.
//!
//! Per `plans/Overview.md` §4, doctor checks daemon, socket, parsers,
//! and code-signing status. Phase 3a probes the daemon socket; daemon
//! supervision (start/stop/status) and code-signing remain Phase-3b /
//! Phase-4 items. Exits 0 even when checks fail — the output is the
//! signal, not the exit code, since `vet doctor` is run interactively.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

pub fn run() -> ExitCode {
    let names = vetter_core::parsers::registered_names();
    let summary = if names.is_empty() {
        "none yet".to_string()
    } else {
        names.join(", ")
    };

    let socket_path = resolve_socket_path();
    let socket_status = match probe_socket(&socket_path) {
        Ok(()) => format!("reachable ({})", socket_path.display()),
        Err(e) => format!("not reachable at {}: {e}", socket_path.display()),
    };

    println!("vet doctor (vetter-core {})", vetter_core::version());
    println!("  daemon ............. {socket_status}");
    println!(
        "  socket ............. path = {} (override: $VETTERD_SOCKET)",
        socket_path.display()
    );
    println!("  parsers registered . {}  ({summary})", names.len());
    println!("  code signing ....... not implemented (Phase 4; see plans/Overview.md §11)");
    ExitCode::SUCCESS
}

fn resolve_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("VETTERD_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = std::env::var_os("TMPDIR").unwrap_or_else(|| std::ffi::OsString::from("/tmp"));
    PathBuf::from(dir).join("vetter.sock")
}

fn probe_socket(p: &std::path::Path) -> std::io::Result<()> {
    let sock = UnixStream::connect(p)?;
    // Don't actually round-trip a frame; we just want to confirm a
    // listener is bound. `connect` succeeding is sufficient.
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    drop(sock);
    Ok(())
}
