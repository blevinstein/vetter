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

use vetter_core::peer_cred::assert_peer_is_self;
use vetter_core::pidfile::{self, PidFileContents};
use vetter_core::wire::{
    read_frame, write_frame, MgmtRequest, MgmtResponse, PendingItem, WireError,
};
use vetter_core::{default_admin_socket_path, default_pidfile_path, default_socket_path};

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

    if !pidfile::is_pid_alive(contents.pid) {
        // Process already gone — clean up the orphan pidfile and
        // report success so retries are idempotent.
        pidfile::remove(&pidfile);
        println!(
            "vet daemon: pid {} no longer running; cleaned up stale pidfile",
            contents.pid
        );
        return ExitCode::from(EXIT_OK);
    }
    let rc = unsafe { kill(contents.pid as i32, SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
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
            let pending_label = match query_admin(MgmtRequest::ListPending) {
                Ok(MgmtResponse::PendingList { items }) => items.len().to_string(),
                _ => "?".to_string(),
            };
            println!(
                "vet daemon: running (pid={}, uptime={}, socket={}, pending={})",
                c.pid,
                format_uptime(&c),
                socket.display(),
                pending_label,
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

/// List pending approval requests waiting for a human decision.
pub fn list() -> ExitCode {
    match query_admin(MgmtRequest::ListPending) {
        Ok(MgmtResponse::PendingList { items }) => {
            print_pending_list(&items);
            ExitCode::from(EXIT_OK)
        }
        Ok(MgmtResponse::Error { message }) => {
            eprintln!("vet daemon list: daemon error: {message}");
            ExitCode::from(EXIT_CONFIG)
        }
        Ok(other) => {
            // The daemon answered with the wrong response variant. This
            // is a protocol-level bug; surface it loudly rather than
            // silently misreporting "no pending approvals".
            eprintln!("vet daemon list: unexpected daemon response: {other:?}");
            ExitCode::from(EXIT_CONFIG)
        }
        Err(e) => {
            eprintln!("vet daemon list: {e}");
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

/// `vet daemon autostart enable` — register Vetter.app as a macOS
/// Login Item so the daemon comes back after every reboot.
pub fn autostart_enable() -> ExitCode {
    autostart_set(true)
}

/// `vet daemon autostart disable` — unregister from Login Items.
pub fn autostart_disable() -> ExitCode {
    autostart_set(false)
}

/// `vet daemon autostart status` — print the current OS-level
/// state and the user's persisted preference.
pub fn autostart_status() -> ExitCode {
    match query_admin(MgmtRequest::GetAutostart) {
        Ok(MgmtResponse::AutostartState { desired, status }) => {
            print_autostart_state(desired, status);
            ExitCode::from(EXIT_OK)
        }
        Ok(MgmtResponse::Error { message }) => {
            eprintln!("vet daemon autostart: daemon error: {message}");
            ExitCode::from(EXIT_CONFIG)
        }
        Ok(other) => {
            eprintln!("vet daemon autostart: unexpected daemon response: {other:?}");
            ExitCode::from(EXIT_CONFIG)
        }
        Err(e) => {
            eprintln!("vet daemon autostart: {e}");
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

fn autostart_set(enabled: bool) -> ExitCode {
    let verb = if enabled { "enable" } else { "disable" };
    match query_admin(MgmtRequest::SetAutostart { enabled }) {
        Ok(MgmtResponse::AutostartState { desired, status }) => {
            print_autostart_state(desired, status);
            ExitCode::from(EXIT_OK)
        }
        Ok(MgmtResponse::Error { message }) => {
            eprintln!("vet daemon autostart {verb}: daemon error: {message}");
            ExitCode::from(EXIT_CONFIG)
        }
        Ok(other) => {
            eprintln!("vet daemon autostart {verb}: unexpected daemon response: {other:?}");
            ExitCode::from(EXIT_CONFIG)
        }
        Err(e) => {
            eprintln!("vet daemon autostart {verb}: {e}");
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

fn print_autostart_state(desired: bool, status: vetter_core::settings::AutostartStatus) {
    use vetter_core::settings::AutostartStatus as S;
    let extra = match status {
        S::Enabled => " (login item registered)",
        S::NotRegistered => " (not registered)",
        S::RequiresApproval => " (waiting for user approval in System Settings → Login Items)",
        S::NotFound => " (the system cannot resolve Vetter.app)",
        S::Unsupported => " (autostart only available on macOS 13+ inside Vetter.app)",
    };
    println!(
        "vet daemon autostart: {label}{extra}\n  preference: autostart = {desired}",
        label = status.label(),
    );
}

fn print_pending_list(items: &[PendingItem]) {
    if items.is_empty() {
        println!("vet daemon: no pending approvals");
        return;
    }
    println!("vet daemon: {} pending approval(s):", items.len());
    for item in items {
        let tag = if item.force_prompt { " [dry-run]" } else { "" };
        // Truncate long targets so output stays readable at 80 cols.
        let target = if item.primary_target.len() > 60 {
            format!("{}…", &item.primary_target[..59])
        } else {
            item.primary_target.clone()
        };
        if item.primary_verb.is_empty() {
            println!("  [{}] {}{} {}", item.id, item.command, tag, target);
        } else {
            println!(
                "  [{}] {} {} {}{}",
                item.id, item.command, item.primary_verb, target, tag
            );
        }
    }
}

/// Send a single [`MgmtRequest`] to the admin socket and return the
/// [`MgmtResponse`]. Returns `Err` with a human-readable message when
/// the admin socket is unreachable (daemon not running, old daemon
/// without admin socket, etc.).
pub(crate) fn query_admin(req: MgmtRequest) -> Result<MgmtResponse, String> {
    let admin_socket = default_admin_socket_path();
    let mut stream = UnixStream::connect(&admin_socket).map_err(|e| {
        format!(
            "cannot connect to admin socket {} (is vetterd running?): {e}",
            admin_socket.display()
        )
    })?;
    assert_peer_is_self(&stream)
        .map_err(|e| format!("admin socket peer-credential check failed: {e}"))?;
    write_frame::<_, MgmtRequest>(&mut stream, &req)
        .map_err(|e: WireError| format!("admin write error: {e}"))?;
    read_frame::<_, MgmtResponse>(&mut stream)
        .map_err(|e: WireError| format!("admin read error: {e}"))
}

fn socket_alive(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(stream) = UnixStream::connect(path) else {
        return false;
    };
    // A reachable socket is only "ours" if the peer runs as us; a
    // same-UID hijacker would otherwise satisfy `connect` and trick
    // `vet daemon start` into reporting success without a real
    // vetterd. (Cross-UID attackers are blocked one layer up by the
    // 0700 runtime dir.)
    assert_peer_is_self(&stream).is_ok()
}

pub(crate) fn locate_vetterd() -> Result<PathBuf, String> {
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
