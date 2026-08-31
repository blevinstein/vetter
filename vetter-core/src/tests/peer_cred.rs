//! Tests for [`crate::peer_cred`]. Layout convention is described in
//! `AGENTS.md`.

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;

use super::*;

extern "C" {
    fn setsid() -> i32;
}

#[test]
fn peer_uid_of_socketpair_is_current_euid() {
    let (a, _b) = UnixStream::pair().expect("socketpair");
    let me = current_euid();
    let peer = peer_uid(&a).expect("getpeereid on connected pair");
    assert_eq!(
        peer, me,
        "socketpair within one process must report self uid"
    );
}

#[test]
fn assert_peer_is_self_passes_for_socketpair() {
    let (a, _b) = UnixStream::pair().expect("socketpair");
    assert_peer_is_self(&a).expect("same-uid pair must pass");
}

#[test]
fn peer_pid_of_socketpair_is_self_pid() {
    // Both ends of a socketpair live in the calling process, so the
    // kernel reports `getpid()` as the peer PID on every platform we
    // build for (`SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS /
    // BSD).
    let (a, _b) = UnixStream::pair().expect("socketpair");
    let me = std::process::id();
    let peer = peer_pid(&a).expect("peer_pid on connected pair");
    assert_eq!(
        peer, me,
        "socketpair within one process must report self pid"
    );
}

#[test]
fn stable_session_for_self_resolves_without_error() {
    let pid = std::process::id();
    stable_session_for(pid).expect("stable_session_for on the test process itself must resolve");
}

/// Spawns a detached grandchild (its own session, no controlling
/// tty — the same shape a fresh shell-tool invocation gets under the
/// harness this was built against) and asserts its ancestor walk
/// converges to the same stable session as the test harness's own
/// walk. This holds regardless of whether the test process itself
/// has a controlling tty (interactive run) or not (headless CI):
/// above the fork point both walks retrace the identical ancestry,
/// so a correct implementation always agrees. See the spike write-up
/// in `plans/Time-limited allowlist rules-*.plan.md` for why the
/// naive `getsid(pid) == pid` check alone is insufficient.
#[test]
fn stable_session_for_detached_child_converges_with_own_walk() {
    let own_pid = std::process::id();
    let own_result = stable_session_for(own_pid).expect("own walk must resolve");

    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("5")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setsid()` takes no arguments, touches no memory
    // shared with the parent, and is safe to call between fork and
    // exec. It detaches the child into a brand-new session with no
    // controlling terminal — deliberately reproducing the
    // "ephemeral session leader" shape the spike found in the real
    // harness.
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn detached child (`sleep`)");
    let child_pid = child.id();

    // Give the child a moment to exec + setsid before we probe its
    // /proc entry, avoiding a race against a still-forking process.
    std::thread::sleep(std::time::Duration::from_millis(100));

    let child_result = stable_session_for(child_pid);
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        child_result.expect("detached child's walk must resolve"),
        own_result,
        "a detached child's ancestor walk must converge to the same \
         stable session as the spawning test process's own walk"
    );
}
