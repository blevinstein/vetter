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
