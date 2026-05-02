//! PID file used by `vet daemon stop|status` to find the running
//! `vetterd`.
//!
//! Format is two ASCII lines so `cat $PIDFILE` is human-readable:
//!
//! ```text
//! <pid>
//! <start_epoch_secs>
//! ```
//!
//! - `pid` is the OS PID of the daemon.
//! - `start_epoch_secs` is `SystemTime::now().duration_since(UNIX_EPOCH)`
//!   captured immediately before the file is written, used by
//!   `vet daemon status` to report uptime without an IPC roundtrip.
//!
//! Writes are atomic via `tempfile::NamedTempFile::persist`, matching
//! the allowlist persistence pattern in
//! [`vetter_core::matcher::loader::write_file`].

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parsed contents of a pidfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidFileContents {
    pub pid: u32,
    pub start: SystemTime,
}

/// Atomically persist `pid` + `start` to `path`. Creates parent dirs
/// as needed (the socket / pidfile dir is usually `$TMPDIR`, which
/// always exists, but tests use ephemeral subdirs).
pub fn write(path: &Path, pid: u32, start: SystemTime) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let secs = start
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let body = format!("{pid}\n{secs}\n");

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = match parent {
        Some(p) => tempfile::NamedTempFile::new_in(p),
        None => tempfile::NamedTempFile::new_in("."),
    }?;
    {
        let mut handle = tmp.as_file();
        handle.write_all(body.as_bytes())?;
        handle.sync_all()?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Read + parse the pidfile at `path`. Returns
/// `io::ErrorKind::NotFound` when the file is missing and
/// `io::ErrorKind::InvalidData` when the contents don't match the
/// two-line schema.
pub fn read(path: &Path) -> io::Result<PidFileContents> {
    let raw = fs::read_to_string(path)?;
    let mut lines = raw.lines();
    let pid_line = lines.next().ok_or_else(|| invalid("missing pid line"))?;
    let start_line = lines.next().ok_or_else(|| invalid("missing start line"))?;
    let pid: u32 = pid_line
        .trim()
        .parse()
        .map_err(|e| invalid(&format!("bad pid: {e}")))?;
    let secs: u64 = start_line
        .trim()
        .parse()
        .map_err(|e| invalid(&format!("bad start: {e}")))?;
    let start = UNIX_EPOCH + Duration::from_secs(secs);
    Ok(PidFileContents { pid, start })
}

/// Best-effort delete. Used in the daemon's shutdown path; failure to
/// delete (e.g. the file was already gone) is not an error worth
/// surfacing to the caller because the next `vetterd` start will
/// reconcile it.
pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("pidfile: {msg}"))
}

#[cfg(test)]
#[path = "tests/pidfile.rs"]
mod tests;
