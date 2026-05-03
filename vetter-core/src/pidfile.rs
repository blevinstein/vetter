//! PID file used by `vet daemon stop|status` to find the running
//! `vetterd` and to attest the daemon's identity to clients.
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
//! ## Locking contract (PID attestation)
//!
//! The daemon holds an *exclusive POSIX write lock* on the file via
//! [`acquire`] for its full lifetime. Closing the underlying fd
//! releases the lock — so when the daemon dies (clean exit, panic,
//! `kill -9`) the kernel guarantees the lock is gone. Clients use
//! [`read_locker_pid`] to ask the kernel which pid currently owns
//! the write lock and refuse to talk to a peer whose pid does not
//! match. See `plans/ThreatModel.md` T1 sequencing #1.
//!
//! We use POSIX `fcntl(F_SETLK)` rather than BSD `flock(2)` because
//! `F_GETLK` is the only portable way to recover the locker's pid.
//! POSIX-lock close-on-any-fd semantics are fine here: the daemon
//! opens the pidfile exactly once at startup, never forks, never
//! re-opens it. Holding [`PidFileLock`] alive is the entire contract.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parsed contents of a pidfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidFileContents {
    pub pid: u32,
    pub start: SystemTime,
}

/// RAII handle for a daemon-side pidfile lock.
///
/// The `File` field keeps the underlying fd alive for the lifetime
/// of the daemon. `Drop` closes the fd, which atomically releases
/// the POSIX write lock — so a daemon that crashes (or is `kill
/// -9`'d) cannot leak a stale lock the way it could leak a stale
/// pidfile.
#[must_use = "the lock is released when this handle is dropped"]
pub struct PidFileLock {
    _file: File,
}

/// Atomically create + lock + populate the pidfile at `path`.
///
/// On success the returned [`PidFileLock`] holds an exclusive POSIX
/// write lock on the file. The body (`pid\nstart_epoch_secs\n`) is
/// written and `fsync`'d before this returns so a concurrent reader
/// observes a fully-formed file or nothing at all.
///
/// Returns:
/// - [`io::ErrorKind::WouldBlock`] when another process already holds
///   the lock (`EAGAIN` / `EACCES` from `fcntl(F_SETLK)`). The caller
///   should treat this as "another vetterd is alive".
/// - Any other [`io::Error`] for IO / mkdir / write failures.
pub fn acquire(path: &Path, pid: u32, start: SystemTime) -> io::Result<PidFileLock> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    // 0600: the pidfile only needs to be readable by the same uid
    // (the client). Open with O_RDWR so F_GETLK from a separate fd
    // in the same process / a child still sees the lock — POSIX is
    // strict that locks are observed at fd granularity, but a
    // F_WRLCK on an O_WRONLY fd cannot be queried via a probe that
    // opens O_RDONLY on macOS either way; both modes work.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    set_write_lock(file.as_raw_fd())?;

    // Truncate now that we own the lock so two daemons fighting for
    // the file can never observe each other's body. Doing this
    // *after* the lock acquisition (rather than via O_TRUNC at open
    // time) means a same-UID racer that opens with O_TRUNC but then
    // fails F_SETLK has no way to clobber our body.
    file.set_len(0)?;
    let secs = start
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let body = format!("{pid}\n{secs}\n");
    {
        let mut handle = &file;
        handle.write_all(body.as_bytes())?;
        handle.sync_all()?;
    }
    Ok(PidFileLock { _file: file })
}

/// Read + parse the pidfile at `path`. Returns
/// [`io::ErrorKind::NotFound`] when the file is missing and
/// [`io::ErrorKind::InvalidData`] when the contents don't match the
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

/// Ask the kernel which pid currently holds the exclusive write
/// lock on `path`.
///
/// Returns:
/// - `Ok(Some(pid))` — a process is holding the lock; `pid` is its
///   OS PID as reported by `fcntl(F_GETLK)`.
/// - `Ok(None)` — the file exists but no process holds the lock
///   (`l_type == F_UNLCK`). This is the "stale pidfile" case the
///   client treats as fail-closed.
/// - `Err(NotFound)` — the file does not exist (no daemon ever
///   started, or the daemon already cleaned up on shutdown).
/// - `Err(_)` — any other IO failure opening / probing the file.
pub fn read_locker_pid(path: &Path) -> io::Result<Option<u32>> {
    let file = OpenOptions::new().read(true).open(path)?;
    query_write_lock(file.as_raw_fd())
}

/// Best-effort delete. Used in the daemon's shutdown path; failure to
/// delete (e.g. the file was already gone) is not an error worth
/// surfacing to the caller because the next `vetterd` start will
/// reconcile it.
pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

/// `kill(pid, 0)` liveness probe: returns `true` if a signal *could*
/// be delivered to `pid` — i.e. the process exists and the caller has
/// permission to signal it. Returns `false` on `ESRCH` ("no such
/// process") and conservatively returns `true` on other errno values
/// (e.g. `EPERM`, which means the process is alive but owned by
/// another uid). Used by `vet daemon stop|status` and `vet doctor` to
/// distinguish a stale pidfile from a live daemon.
pub fn is_pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let rc = unsafe { kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    let err = io::Error::last_os_error();
    // ESRCH (3 on Linux + macOS) is the only "definitely dead" answer;
    // every other errno (EPERM, EINVAL, …) means we can't say the
    // process is gone, so keep the caller honest by returning true.
    err.raw_os_error() != Some(3)
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("pidfile: {msg}"))
}

// ---------- platform-specific fcntl(2) wrappers ----------

// `fcntl(2)` is variadic (`int fcntl(int fd, int cmd, ...)`). On
// AArch64 macOS in particular, variadic and non-variadic calls use
// different calling conventions: a non-variadic prototype passes
// args in registers, while the variadic prototype lays them out per
// the AAPCS64 variadic rules (effectively forced through the stack
// for some types). Declaring `fcntl` non-variadic here would silently
// corrupt the third argument on Apple Silicon — manifesting as
// `EFAULT` / `EINVAL` from the kernel because the `struct flock *`
// pointer never lands where libc expects it.
extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
}

// Only F_WRLCK / F_UNLCK are wired up; we never take shared (read)
// locks on the pidfile so F_RDLCK is intentionally absent.
#[cfg(target_os = "linux")]
mod fcntl_consts {
    pub const F_GETLK: i32 = 5;
    pub const F_SETLK: i32 = 6;
    pub const F_WRLCK: i16 = 1;
    pub const F_UNLCK: i16 = 2;
}

#[cfg(not(target_os = "linux"))]
mod fcntl_consts {
    pub const F_GETLK: i32 = 7;
    pub const F_SETLK: i32 = 8;
    pub const F_UNLCK: i16 = 2;
    pub const F_WRLCK: i16 = 3;
}

const SEEK_SET: i16 = 0;

#[cfg(target_os = "linux")]
#[repr(C)]
struct Flock {
    l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    l_pid: i32,
}

#[cfg(target_os = "linux")]
fn new_flock(l_type: i16) -> Flock {
    Flock {
        l_type,
        l_whence: SEEK_SET,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    }
}

#[cfg(not(target_os = "linux"))]
#[repr(C)]
struct Flock {
    l_start: i64,
    l_len: i64,
    l_pid: i32,
    l_type: i16,
    l_whence: i16,
}

#[cfg(not(target_os = "linux"))]
fn new_flock(l_type: i16) -> Flock {
    Flock {
        l_start: 0,
        l_len: 0,
        l_pid: 0,
        l_type,
        l_whence: SEEK_SET,
    }
}

fn set_write_lock(fd: i32) -> io::Result<()> {
    use fcntl_consts::*;
    let mut fl = new_flock(F_WRLCK);
    let rc = unsafe { fcntl(fd, F_SETLK, &mut fl as *mut Flock as *mut std::ffi::c_void) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // POSIX: F_SETLK reports EAGAIN or EACCES when another process
    // holds an incompatible lock. Map both onto WouldBlock so callers
    // have one error-kind to switch on.
    match err.raw_os_error() {
        // EAGAIN: 11 on Linux, 35 on macOS. EACCES: 13 on both.
        Some(11) | Some(13) | Some(35) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "pidfile is locked by another process",
        )),
        _ => Err(err),
    }
}

fn query_write_lock(fd: i32) -> io::Result<Option<u32>> {
    use fcntl_consts::*;
    let mut fl = new_flock(F_WRLCK);
    let rc = unsafe { fcntl(fd, F_GETLK, &mut fl as *mut Flock as *mut std::ffi::c_void) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if fl.l_type == F_UNLCK {
        return Ok(None);
    }
    if fl.l_pid <= 0 {
        // Some kernels return 0 for OFD locks or for kernel-internal
        // locks where the owner pid is meaningless. Treat that as
        // "locked by someone we can't name" → None so the client
        // fails closed instead of silently allowing a 0-pid match.
        return Ok(None);
    }
    Ok(Some(fl.l_pid as u32))
}

#[cfg(test)]
#[path = "tests/pidfile.rs"]
mod tests;
