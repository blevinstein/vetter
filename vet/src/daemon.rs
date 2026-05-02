//! `vet daemon start | stop | status` — supervise the long-running
//! `vetterd` process from the user's CLI.
//!
//! Design notes:
//!
//! - **Self-detach**: `start` spawns `vetterd` with stdio redirected
//!   to `/dev/null` and `setsid` called via `pre_exec` so the child
//!   leaves the controlling terminal behind. This survives a parent
//!   shell hanging up, without depending on launchd / systemd.
//! - **Pidfile** is owned by `vetterd` and read by `stop`/`status`.
//!   Format: see [`vetter_core::pidfile`]. Co-located with the socket
//!   by default so per-test scratch dirs stay isolated.
//! - **Binary discovery** for `start`: `$VETTERD_BIN` → sibling of
//!   the running `vet` (covers Homebrew installs and Cargo `target/`)
//!   → first `vetterd` on `PATH`.

use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime};

use vetter_core::pidfile::{self, PidFileContents};
use vetter_core::{default_pidfile_path, default_socket_path};

const EXIT_OK: u8 = 0;
const EXIT_CONFIG: u8 = 78;

const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

const SIGTERM: i32 = 15;

pub fn start() -> ExitCode {
    let socket = default_socket_path();
    let pidfile = default_pidfile_path(&socket);

    if socket_alive(&socket) {
        let pid_msg = match pidfile::read(&pidfile) {
            Ok(p) => format!(" (pid={})", p.pid),
            Err(_) => String::new(),
        };
        println!(
            "vet daemon: already running{pid_msg}, listening on {}",
            socket.display()
        );
        return ExitCode::from(EXIT_OK);
    }

    let bin = match locate_vetterd() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("vet daemon: cannot locate vetterd binary: {e}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    let mut cmd = Command::new(&bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid()` is async-signal-safe and the closure does
    // not allocate. `pre_exec` runs in the child after fork, before
    // exec, which is exactly the window we need to leave the
    // controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            if setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("vet daemon: spawn `{}` failed: {e}", bin.display());
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    let child_pid = child.id();

    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        // Both signals must agree: the socket is the "I can serve
        // requests" gate, the pidfile is the "stop / status can find
        // me" gate. vetterd writes the pidfile after binding, so
        // polling both rules out the race where the socket is up but
        // the pidfile isn't yet on disk.
        if socket_alive(&socket) && pidfile.exists() {
            let pid_label = pidfile::read(&pidfile).map(|p| p.pid).unwrap_or(child_pid);
            println!(
                "vet daemon: started (pid={pid_label}, socket={})",
                socket.display()
            );
            return ExitCode::from(EXIT_OK);
        }
        // If the child exited before binding (bad allowlist, missing
        // dirs, ...), bail rather than block until the deadline.
        match child.try_wait() {
            Ok(Some(status)) => {
                eprintln!(
                    "vet daemon: vetterd exited before binding socket: {status} \
                     (run `vetterd` directly to see its error output)"
                );
                return ExitCode::from(EXIT_CONFIG);
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("vet daemon: cannot poll vetterd child: {e}");
                return ExitCode::from(EXIT_CONFIG);
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    // Timed out. Clean up the orphan child so we don't leave a
    // half-started daemon behind.
    unsafe {
        kill(child_pid as i32, SIGTERM);
    }
    let _ = child.wait();
    eprintln!(
        "vet daemon: vetterd did not bind {} within {}s",
        socket.display(),
        START_TIMEOUT.as_secs()
    );
    ExitCode::from(EXIT_CONFIG)
}

pub fn stop() -> ExitCode {
    let socket = default_socket_path();
    let pidfile = default_pidfile_path(&socket);

    let contents = match pidfile::read(&pidfile) {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            println!(
                "vet daemon: no daemon running (no pidfile at {})",
                pidfile.display()
            );
            return ExitCode::from(EXIT_OK);
        }
        Err(e) => {
            eprintln!("vet daemon: cannot read pidfile {}: {e}", pidfile.display());
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    let rc = unsafe { kill(contents.pid as i32, SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc_esrch()) {
            // Process already gone — clean up the orphan pidfile and
            // report success so retries are idempotent.
            pidfile::remove(&pidfile);
            println!(
                "vet daemon: pid {} no longer running; cleaned up stale pidfile",
                contents.pid
            );
            return ExitCode::from(EXIT_OK);
        }
        eprintln!("vet daemon: SIGTERM pid {} failed: {err}", contents.pid);
        return ExitCode::from(EXIT_CONFIG);
    }

    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !socket_alive(&socket) && !pidfile.exists() {
            println!("vet daemon: stopped (pid={})", contents.pid);
            return ExitCode::from(EXIT_OK);
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    eprintln!(
        "vet daemon: pid {} did not exit within {}s; \
         try `kill -9 {}` if it remains stuck",
        contents.pid,
        STOP_TIMEOUT.as_secs(),
        contents.pid
    );
    ExitCode::from(EXIT_CONFIG)
}

pub fn status() -> ExitCode {
    let socket = default_socket_path();
    let pidfile = default_pidfile_path(&socket);
    let contents = pidfile::read(&pidfile).ok();
    let alive = socket_alive(&socket);

    match (contents, alive) {
        (Some(c), true) => {
            println!(
                "vet daemon: running (pid={}, uptime={}, socket={}, pending=0 [Phase 4 will populate])",
                c.pid,
                format_uptime(&c),
                socket.display()
            );
            ExitCode::from(EXIT_OK)
        }
        (Some(c), false) => {
            eprintln!(
                "vet daemon: stale pidfile at {} (pid={} but socket {} not responding)",
                pidfile.display(),
                c.pid,
                socket.display()
            );
            ExitCode::from(EXIT_CONFIG)
        }
        (None, true) => {
            eprintln!(
                "vet daemon: socket {} is responding but no pidfile at {}; \
                 daemon may have been started outside `vet daemon start`",
                socket.display(),
                pidfile.display()
            );
            ExitCode::from(EXIT_CONFIG)
        }
        (None, false) => {
            println!("vet daemon: not running (no pidfile, no socket)");
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

fn socket_alive(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    UnixStream::connect(path).is_ok()
}

fn locate_vetterd() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("VETTERD_BIN") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
        return Err(format!(
            "$VETTERD_BIN points at {} which does not exist",
            pb.display()
        ));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("vetterd");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    if let Some(p) = which_in_path("vetterd") {
        return Ok(p);
    }
    Err("looked at $VETTERD_BIN, sibling of `vet`, and PATH; \
         install vetterd or set $VETTERD_BIN"
        .into())
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn format_uptime(c: &PidFileContents) -> String {
    let now = SystemTime::now();
    let dur = now.duration_since(c.start).unwrap_or_default();
    let total = dur.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h{m}m{s}s")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

/// `ESRCH` ("no such process") in libc, hard-coded to avoid pulling
/// in the libc crate. Same value across Linux + macOS.
fn libc_esrch() -> i32 {
    3
}
