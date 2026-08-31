//! Unix-socket peer-credential check.
//!
//! `vet` connects to a path that, by definition, lives in user-
//! writable space. Even when the socket sits in `$XDG_RUNTIME_DIR`
//! (0700, cross-user safe), a *same-UID* process — exactly the
//! aligned-but-fallible agent context the threat model worries about
//! — can race `vetterd` for the bind, accept the client's
//! [`crate::wire::VetRequest`], dump it, and return a forged
//! `Allow`. Closing that gap requires asking the kernel "who is on
//! the other end of this stream?" and refusing to talk if the answer
//! is not us.
//!
//! Implementation is platform-specific:
//! - **Linux**: `getsockopt(SO_PEERCRED)` returns a `struct ucred`
//!   filled in with the peer's pid / uid / gid at `connect`/`accept`
//!   time.
//! - **macOS / BSD**: `getpeereid(2)` returns euid + egid directly.
//!
//! We deliberately do *not* pull in the `libc` crate (the rest of
//! the workspace links the few syscalls it needs directly via
//! `extern "C"`); the `extern` blocks below mirror the platform
//! prototypes.

use std::os::unix::io::AsRawFd;

use crate::wire::WireError;

extern "C" {
    fn geteuid() -> u32;
    // POSIX `getsid(2)`. Same signature on Linux and macOS (`pid_t`
    // is `i32` on both). Unlike the peer-cred socket options above,
    // this isn't scoped to a connected stream — it queries the
    // session id of an arbitrary already-peer-cred-verified pid, so
    // it lives in the shared extern block rather than either
    // platform module.
    fn getsid(pid: i32) -> i32;
}

/// Effective UID of the calling process. Cheap (one syscall) so we
/// re-fetch on every connect rather than caching across calls.
pub fn current_euid() -> u32 {
    unsafe { geteuid() }
}

/// Read the peer UID off `stream`. Errors are mapped to
/// [`WireError::Io`] using `last_os_error()` so the caller can
/// surface them through the same error path as a normal framing
/// failure.
pub fn peer_uid<S: AsRawFd>(stream: &S) -> Result<u32, WireError> {
    peer_uid_impl(stream.as_raw_fd())
}

/// Read the peer PID off `stream`.
///
/// Used by the PID-attestation step: peer-cred (UID) catches a
/// different-UID attacker; combining the peer PID with
/// `pidfile::read_locker_pid` is what catches a same-UID racer that
/// binds the socket before the legitimate `vetterd` does. See
/// `plans/ThreatModel.md` T1.
///
/// - **Linux**: re-runs `getsockopt(SO_PEERCRED)` and returns
///   `cred.pid`. One extra syscall per connect over `peer_uid` —
///   negligible compared to the network round-trip that follows.
/// - **macOS / BSD**: uses `getsockopt(SOL_LOCAL, LOCAL_PEERPID)`
///   which returns `pid_t` directly. `getpeereid(2)` only exposes
///   euid / egid so we cannot piggy-back on it.
pub fn peer_pid<S: AsRawFd>(stream: &S) -> Result<u32, WireError> {
    peer_pid_impl(stream.as_raw_fd())
}

/// Assert that the peer on the other end of `stream` runs as the
/// calling process. Returns [`WireError::PeerAuth`] on mismatch so
/// `vet` can fail closed with a recognisable error rather than a
/// generic IO failure.
pub fn assert_peer_is_self<S: AsRawFd>(stream: &S) -> Result<(), WireError> {
    let expected = current_euid();
    let peer = peer_uid(stream)?;
    if peer != expected {
        return Err(WireError::PeerAuth { expected, peer });
    }
    Ok(())
}

/// Maximum number of ancestor-session hops before giving up. PPID
/// chains are kernel-reported, not client-controlled, but a
/// pathological or unusually deep process tree (or a bug in the walk
/// itself) must not be able to hang the daemon on a connect.
const MAX_SESSION_WALK_DEPTH: usize = 32;

/// Resolve the "stable session" for `pid`: the session id of the
/// nearest ancestor (by session leader) that owns a controlling TTY.
///
/// Naively calling `getsid(pid)` is not useful for session-scoped
/// allowlist rules under harnesses that spawn a fresh session leader
/// for every shell-tool invocation (confirmed live against the
/// Cursor CLI agent shell tool — see
/// `plans/Time-limited allowlist rules-*.plan.md` for the spike
/// write-up): every call gets its own ephemeral sid that is never
/// reused, so a rule scoped to it would only ever match once.
/// Walking up to the nearest TTY-anchored ancestor's session
/// resolves to the same stable id (typically the login shell's
/// session in the terminal tab) across independent invocations from
/// the same terminal.
///
/// Algorithm, one hop at a time starting from `pid`:
/// 1. `sid = getsid(pid)`.
/// 2. If the session leader (`sid` is itself a pid) has a
///    controlling tty, `sid` is stable — return it.
/// 3. Otherwise walk to the session leader's *parent* (not `pid`'s
///    parent) and repeat.
///
/// This runs entirely server-side against the kernel-reported PPID
/// chain of a connection whose peer pid was already verified via
/// [`peer_pid`]/[`assert_peer_is_self`], so it carries the same trust
/// level — a client cannot forge its own ancestry.
///
/// Returns `Err` if any step fails (a process in the chain exited
/// mid-walk, a permission error, or the walk exceeds
/// [`MAX_SESSION_WALK_DEPTH`] without finding a tty-anchored
/// ancestor). Callers map `Err` to "session-scoped rules unavailable
/// for this request" rather than propagating a hard failure.
pub fn stable_session_for(pid: u32) -> Result<i32, WireError> {
    let mut current = i32::try_from(pid).map_err(|_| {
        WireError::Io(std::io::Error::other(format!(
            "pid {pid} does not fit in pid_t"
        )))
    })?;
    for _ in 0..MAX_SESSION_WALK_DEPTH {
        let sid = getsid_checked(current)?;
        if platform::has_controlling_tty(sid)? {
            return Ok(sid);
        }
        match platform::ppid_of(sid)? {
            Some(parent) if parent > 1 => current = parent,
            _ => return Ok(sid),
        }
    }
    Err(WireError::Io(std::io::Error::other(format!(
        "stable_session_for({pid}): exceeded walk depth {MAX_SESSION_WALK_DEPTH} \
         without finding a tty-anchored ancestor session"
    ))))
}

fn getsid_checked(pid: i32) -> Result<i32, WireError> {
    let sid = unsafe { getsid(pid) };
    if sid < 0 {
        return Err(WireError::Io(std::io::Error::last_os_error()));
    }
    Ok(sid)
}

#[cfg(target_os = "linux")]
mod platform {
    use super::WireError;

    // `struct ucred` from <bits/socket.h>: pid_t, uid_t, gid_t.
    // Field order is part of the kernel ABI.
    #[repr(C)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }

    extern "C" {
        fn getsockopt(
            socket: i32,
            level: i32,
            name: i32,
            value: *mut std::ffi::c_void,
            option_len: *mut u32,
        ) -> i32;
    }

    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;

    fn read_ucred(fd: i32) -> Result<Ucred, WireError> {
        let mut cred = Ucred {
            pid: 0,
            uid: u32::MAX,
            gid: u32::MAX,
        };
        let mut len = std::mem::size_of::<Ucred>() as u32;
        let rc = unsafe {
            getsockopt(
                fd,
                SOL_SOCKET,
                SO_PEERCRED,
                &mut cred as *mut Ucred as *mut std::ffi::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(WireError::Io(std::io::Error::last_os_error()));
        }
        Ok(cred)
    }

    pub(super) fn peer_uid(fd: i32) -> Result<u32, WireError> {
        Ok(read_ucred(fd)?.uid)
    }

    pub(super) fn peer_pid(fd: i32) -> Result<u32, WireError> {
        // Linux ucred.pid is `pid_t` (i32). Negative values aren't
        // legal here, but be defensive: treat a negative pid as an IO
        // failure rather than wrapping into u32::MAX-ish nonsense.
        let pid = read_ucred(fd)?.pid;
        if pid < 0 {
            return Err(WireError::Io(std::io::Error::other(format!(
                "SO_PEERCRED returned negative pid {pid}"
            ))));
        }
        Ok(pid as u32)
    }

    /// Fields 4 (`ppid`) and 7 (`tty_nr`) of `/proc/[pid]/stat`
    /// (`proc(5)`). Parsed together since both primitives need the
    /// same file; each public function below just reads what it
    /// needs off one parse.
    ///
    /// The `comm` field (field 2) is parenthesised and may itself
    /// contain `)` (a process can rename itself to anything), so we
    /// skip to the **last** `)` in the line before splitting the
    /// remaining space-separated fields — the same defensive
    /// convention every `/proc/[pid]/stat` parser needs.
    struct ProcStat {
        ppid: i32,
        tty_nr: i32,
    }

    fn read_proc_stat(pid: i32) -> Result<ProcStat, WireError> {
        let path = format!("/proc/{pid}/stat");
        let content = std::fs::read_to_string(&path)?;
        let close = content.rfind(')').ok_or_else(|| {
            WireError::Io(std::io::Error::other(format!(
                "malformed {path}: no `)` found closing the comm field"
            )))
        })?;
        let mut fields = content[close + 1..].split_whitespace();
        let malformed = || {
            WireError::Io(std::io::Error::other(format!(
                "malformed {path}: fewer fields than expected after comm"
            )))
        };
        let _state = fields.next().ok_or_else(malformed)?;
        let ppid: i32 = fields
            .next()
            .ok_or_else(malformed)?
            .parse()
            .map_err(|_| malformed())?;
        let _pgrp = fields.next().ok_or_else(malformed)?;
        let _session = fields.next().ok_or_else(malformed)?;
        let tty_nr: i32 = fields
            .next()
            .ok_or_else(malformed)?
            .parse()
            .map_err(|_| malformed())?;
        Ok(ProcStat { ppid, tty_nr })
    }

    pub(super) fn ppid_of(pid: i32) -> Result<Option<i32>, WireError> {
        Ok(Some(read_proc_stat(pid)?.ppid))
    }

    /// `tty_nr` is 0 iff the process has no controlling terminal.
    pub(super) fn has_controlling_tty(pid: i32) -> Result<bool, WireError> {
        Ok(read_proc_stat(pid)?.tty_nr != 0)
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::WireError;

    extern "C" {
        fn getpeereid(s: i32, euid: *mut u32, egid: *mut u32) -> i32;
        fn getsockopt(
            socket: i32,
            level: i32,
            name: i32,
            value: *mut std::ffi::c_void,
            option_len: *mut u32,
        ) -> i32;
    }

    pub(super) fn peer_uid(fd: i32) -> Result<u32, WireError> {
        let mut uid: u32 = 0;
        let mut gid: u32 = 0;
        let rc = unsafe { getpeereid(fd, &mut uid, &mut gid) };
        if rc != 0 {
            return Err(WireError::Io(std::io::Error::last_os_error()));
        }
        Ok(uid)
    }

    // macOS / Darwin socket-option for peer pid on a Unix domain
    // socket. SOL_LOCAL is the level; LOCAL_PEERPID returns the peer's
    // `pid_t` (i32). Both available since 10.8 and stable in the
    // `xnu` ABI.
    const SOL_LOCAL: i32 = 0;
    const LOCAL_PEERPID: i32 = 0x002;

    pub(super) fn peer_pid(fd: i32) -> Result<u32, WireError> {
        let mut pid: i32 = 0;
        let mut len = std::mem::size_of::<i32>() as u32;
        let rc = unsafe {
            getsockopt(
                fd,
                SOL_LOCAL,
                LOCAL_PEERPID,
                &mut pid as *mut i32 as *mut std::ffi::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(WireError::Io(std::io::Error::last_os_error()));
        }
        if pid < 0 {
            return Err(WireError::Io(std::io::Error::other(format!(
                "LOCAL_PEERPID returned negative pid {pid}"
            ))));
        }
        Ok(pid as u32)
    }

    // `proc_pidinfo(3)` from libSystem (no extra linking needed —
    // every macOS binary already links against it). We deliberately
    // read `struct proc_bsdinfo` via `PROC_PIDTBSDINFO` rather than
    // `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID, ...)`'s
    // `struct kinfo_proc`: `kinfo_proc` embeds several kernel-
    // internal, pointer-sized, non-portable-across-builds structs
    // (`extern_proc`, `vmspace`, …) before the fields we actually
    // want, so hand-rolling its layout is fragile. `proc_bsdinfo` is
    // Apple's own public, fixed-width, documented ABI (declared in
    // `<sys/proc_info.h>`) and conveniently carries both the parent
    // pid *and* the controlling-tty flag in one call, so both
    // primitives below share one syscall.
    extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut std::ffi::c_void,
            buffersize: i32,
        ) -> i32;
    }

    const PROC_PIDTBSDINFO: i32 = 3;
    /// `PROC_FLAG_CTTY` from `<sys/proc_info.h>`: set when the
    /// process has a controlling terminal.
    const PROC_FLAG_CTTY: u32 = 0x0100;
    const MAXCOMLEN: usize = 16;

    /// Mirrors `struct proc_bsdinfo` field-for-field (every field is
    /// a fixed-width integer or byte array, so `repr(C)` reproduces
    /// the C layout without needing `#[repr(packed)]` tricks).
    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [u8; MAXCOMLEN],
        pbi_name: [u8; 2 * MAXCOMLEN],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    fn proc_bsdinfo(pid: i32) -> Result<ProcBsdInfo, WireError> {
        // SAFETY: `ProcBsdInfo` is a plain-old-data struct of
        // fixed-width integers / byte arrays; zeroed is a valid
        // initial value for every field.
        let mut info: ProcBsdInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ProcBsdInfo>() as i32;
        let rc = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTBSDINFO,
                0,
                &mut info as *mut ProcBsdInfo as *mut std::ffi::c_void,
                size,
            )
        };
        if rc <= 0 {
            return Err(WireError::Io(std::io::Error::last_os_error()));
        }
        Ok(info)
    }

    pub(super) fn ppid_of(pid: i32) -> Result<Option<i32>, WireError> {
        Ok(Some(proc_bsdinfo(pid)?.pbi_ppid as i32))
    }

    pub(super) fn has_controlling_tty(pid: i32) -> Result<bool, WireError> {
        Ok(proc_bsdinfo(pid)?.pbi_flags & PROC_FLAG_CTTY != 0)
    }
}

fn peer_uid_impl(fd: i32) -> Result<u32, WireError> {
    platform::peer_uid(fd)
}

fn peer_pid_impl(fd: i32) -> Result<u32, WireError> {
    platform::peer_pid(fd)
}

#[cfg(test)]
#[path = "tests/peer_cred.rs"]
mod tests;
