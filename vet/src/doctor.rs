//! `vet doctor` — interactive diagnostic.
//!
//! Cross-checks daemon liveness (pidfile parses → live PID → socket
//! connectable → peer-cred matches self), socket + parent directory
//! perms, audit-log writability, allowlist parse, and the registered
//! parser set. Exits 78 if any check is in the ERROR class, 0
//! otherwise (per `plans/Overview.md` §4 / `plans/TestingPlan.md`
//! §4.7). WARN-class problems are reported but do not change the
//! exit code.
//!
//! The motivating case is the orphan-pidfile-or-socket combination
//! we hit during Phase 4 development: `connect()` succeeds against a
//! same-UID hijacker, or a `vet daemon start` ago left a pidfile
//! pointing at a long-gone PID. The earlier `doctor` stub treated
//! either as "all good" because its only daemon probe was
//! `UnixStream::connect`. This version walks the same state machine
//! `vet daemon status` does and adds the perm + config checks.

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use vetter_core::matcher::{
    self, discover_project_root, user_allowlist_path, AllowlistFile, LoadError,
};
use vetter_core::peer_cred::{current_euid, peer_uid};
use vetter_core::pidfile::{self, PidFileContents};
use vetter_core::{default_audit_path, default_pidfile_path, default_socket_path};

/// `EX_CONFIG`-equivalent per `plans/Overview.md` §4.
const EXIT_CONFIG: u8 = 78;

/// Per-check verdict. `Ok` is the only "all clear"; `Warn` and
/// `Error` carry an actionable message; `Info` is "nothing wrong but
/// also nothing to verify" (e.g. daemon legitimately not running);
/// `Skip` is "this check is intentionally unimplemented" (e.g. code
/// signing) or "not applicable" (e.g. project allowlist where no
/// project root was found).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Error,
    Info,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Info => "INFO",
            Self::Skip => "SKIP",
        }
    }
}

struct Check {
    name: &'static str,
    status: Status,
    detail: String,
}

impl Check {
    fn new(name: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
        }
    }
}

/// Entry point. `allowlist_override` mirrors the `--allowlist <path>`
/// global flag: when set, it becomes the sole allowlist source for
/// the parse check (matching the daemon's own override semantics in
/// `vetterd::run`).
pub fn run(allowlist_override: Option<&Path>) -> ExitCode {
    let socket_path = default_socket_path();
    let pidfile_path = default_pidfile_path(&socket_path);
    let audit_path_result = default_audit_path();
    let cwd = std::env::current_dir().ok();

    let mut checks: Vec<Check> = vec![
        check_daemon(&pidfile_path, &socket_path),
        check_socket(&socket_path),
        check_socket_parent_dir(&socket_path),
        check_pidfile(&pidfile_path),
        check_audit_log(audit_path_result.as_ref()),
    ];
    checks.extend(check_allowlists(cwd.as_deref(), allowlist_override));
    checks.push(check_parsers());
    checks.push(check_code_signing());

    print_report(&checks);

    let errors = checks.iter().filter(|c| c.status == Status::Error).count();
    if errors > 0 {
        ExitCode::from(EXIT_CONFIG)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_report(checks: &[Check]) {
    println!("vet doctor (vetter-core {})", vetter_core::version());
    // Pad each label out with dots to a fixed width so the status
    // column lines up. The +1 leaves a single space between the
    // label text and the start of the dotted run.
    let label_pad = checks.iter().map(|c| c.name.len()).max().unwrap_or(0) + 2;
    for c in checks {
        let mut label = format!("{} ", c.name);
        while label.len() < label_pad {
            label.push('.');
        }
        println!("  {label} {:<5} {}", c.status.label(), c.detail);
    }

    let errors = checks.iter().filter(|c| c.status == Status::Error).count();
    let warnings = checks.iter().filter(|c| c.status == Status::Warn).count();
    println!();
    println!(
        "summary: {errors} error{}, {warnings} warning{}",
        plural(errors),
        plural(warnings),
    );
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Walk the daemon-liveness state machine documented in the plan:
///
/// - both absent              → INFO not running (clean state).
/// - socket only              → ERROR (orphan socket).
/// - pidfile corrupt          → ERROR.
/// - pidfile, dead PID        → ERROR (stale pidfile).
/// - pidfile, alive, no sock  → ERROR (pidfile present but socket missing).
/// - all three, peer foreign  → ERROR (hijack).
/// - all three, peer self     → OK (pid + uptime + peer_uid).
fn check_daemon(pidfile_path: &Path, socket_path: &Path) -> Check {
    let pid_read = pidfile::read(pidfile_path);
    let socket_present = socket_path.exists();

    match pid_read {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if socket_present {
                Check::new(
                    "daemon",
                    Status::Error,
                    format!(
                        "socket {} present but no pidfile at {} \
                         (orphan socket; remove and run `vet daemon start`)",
                        socket_path.display(),
                        pidfile_path.display()
                    ),
                )
            } else {
                Check::new(
                    "daemon",
                    Status::Info,
                    "not running (no pidfile, no socket). Start with `vet daemon start`.",
                )
            }
        }
        Err(e) => Check::new(
            "daemon",
            Status::Error,
            format!("cannot read pidfile {}: {e}", pidfile_path.display()),
        ),
        Ok(contents) => check_daemon_with_pidfile(contents, pidfile_path, socket_path),
    }
}

fn check_daemon_with_pidfile(
    contents: PidFileContents,
    pidfile_path: &Path,
    socket_path: &Path,
) -> Check {
    if !pidfile::is_pid_alive(contents.pid) {
        return Check::new(
            "daemon",
            Status::Error,
            format!(
                "stale pidfile: pid {} from {} is not running. \
                 Run `vet daemon stop` to clean up, then `vet daemon start`.",
                contents.pid,
                pidfile_path.display()
            ),
        );
    }

    let stream = match UnixStream::connect(socket_path) {
        Ok(s) => s,
        Err(e) => {
            return Check::new(
                "daemon",
                Status::Error,
                format!(
                    "pidfile points at live pid {} but socket {} is unreachable ({e}). \
                     The daemon may be wedged; SIGTERM and restart it.",
                    contents.pid,
                    socket_path.display()
                ),
            );
        }
    };

    let expected = current_euid();
    let peer = match peer_uid(&stream) {
        Ok(uid) => uid,
        Err(e) => {
            return Check::new(
                "daemon",
                Status::Error,
                format!(
                    "cannot read peer uid on {} ({e}); refusing to declare the daemon healthy",
                    socket_path.display()
                ),
            );
        }
    };
    drop(stream);

    if peer != expected {
        return Check::new(
            "daemon",
            Status::Error,
            format!(
                "socket peer uid={peer}, expected uid={expected}. \
                 Another user (or a process running as another user) is bound to {}. \
                 Stop it and restart your own daemon.",
                socket_path.display()
            ),
        );
    }

    Check::new(
        "daemon",
        Status::Ok,
        format!(
            "pid={}, uptime={}, peer_uid={peer}",
            contents.pid,
            format_uptime(contents.start),
        ),
    )
}

fn check_socket(socket_path: &Path) -> Check {
    let meta = match std::fs::metadata(socket_path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Check::new(
                "socket",
                Status::Skip,
                format!("absent ({})", socket_path.display()),
            );
        }
        Err(e) => {
            return Check::new(
                "socket",
                Status::Error,
                format!("stat {}: {e}", socket_path.display()),
            );
        }
    };
    let mode = meta.permissions().mode() & 0o777;
    let owner = meta.uid();
    let self_uid = current_euid();
    let owner_label = if owner == self_uid {
        "self".to_string()
    } else {
        format!("uid={owner}")
    };
    let detail = format!(
        "{} (mode 0{mode:o}, owner {owner_label})",
        socket_path.display()
    );
    if mode == 0o600 && owner == self_uid {
        Check::new("socket", Status::Ok, detail)
    } else if owner != self_uid {
        Check::new(
            "socket",
            Status::Error,
            format!("{detail} (expected owner self)"),
        )
    } else {
        Check::new(
            "socket",
            Status::Warn,
            format!("{detail} (expected mode 0600)"),
        )
    }
}

fn check_socket_parent_dir(socket_path: &Path) -> Check {
    let parent = match socket_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => p,
        None => {
            return Check::new(
                "socket parent dir",
                Status::Skip,
                "socket path has no parent (using TMPDIR fallback?)",
            );
        }
    };
    let meta = match std::fs::metadata(parent) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Check::new(
                "socket parent dir",
                Status::Info,
                format!(
                    "absent ({}); will be created with mode 0700 on next `vet daemon start`",
                    parent.display()
                ),
            );
        }
        Err(e) => {
            return Check::new(
                "socket parent dir",
                Status::Error,
                format!("stat {}: {e}", parent.display()),
            );
        }
    };
    let mode = meta.permissions().mode() & 0o777;
    let owner = meta.uid();
    let self_uid = current_euid();
    let owner_label = if owner == self_uid {
        "self".to_string()
    } else {
        format!("uid={owner}")
    };
    let detail = format!("{} (mode 0{mode:o}, owner {owner_label})", parent.display());
    if mode == 0o700 && owner == self_uid {
        Check::new("socket parent dir", Status::Ok, detail)
    } else if owner != self_uid {
        Check::new(
            "socket parent dir",
            Status::Error,
            format!("{detail} (expected owner self)"),
        )
    } else {
        Check::new(
            "socket parent dir",
            Status::Warn,
            format!("{detail} (expected mode 0700)"),
        )
    }
}

fn check_pidfile(pidfile_path: &Path) -> Check {
    if pidfile_path.exists() {
        Check::new(
            "pidfile",
            Status::Ok,
            format!("present ({})", pidfile_path.display()),
        )
    } else {
        Check::new(
            "pidfile",
            Status::Info,
            format!("absent ({})", pidfile_path.display()),
        )
    }
}

/// Probe the audit log path with `OpenOptions::append + create`.
/// Successful open is the test; we close immediately. We deliberately
/// do not pre-create the parent directory — `vetterd` does that on
/// startup, and it's a useful signal if the parent is missing
/// (`audit log: ERROR No such file or directory`) rather than
/// silently materialising a fresh tree on `vet doctor`.
fn check_audit_log(audit_path: Result<&PathBuf, &vetter_core::paths::PathError>) -> Check {
    let path = match audit_path {
        Ok(p) => p,
        Err(e) => {
            return Check::new("audit log", Status::Error, format!("{e}"));
        }
    };
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(_) => Check::new(
            "audit log",
            Status::Ok,
            format!("{} (writable)", path.display()),
        ),
        Err(e) => Check::new(
            "audit log",
            Status::Error,
            format!("{}: {e}", path.display()),
        ),
    }
}

/// Three checks, depending on what's configured: user, project,
/// override. Always emit the user + project rows so a `--allowlist`
/// override doesn't hide a broken user file (the daemon honours
/// `VETTER_ALLOWLIST` independently of `vet`'s `--allowlist`, so
/// they aren't always the same source).
fn check_allowlists(cwd: Option<&Path>, override_path: Option<&Path>) -> Vec<Check> {
    let mut checks = Vec::new();
    checks.push(check_user_allowlist());
    checks.push(check_project_allowlist(cwd));
    if let Some(p) = override_path {
        checks.push(check_override_allowlist(p));
    }
    checks
}

fn check_user_allowlist() -> Check {
    let path = match user_allowlist_path() {
        Some(p) => p,
        None => {
            return Check::new(
                "allowlist (user)",
                Status::Warn,
                "$HOME is not set; cannot resolve user allowlist path",
            );
        }
    };
    if !path.exists() {
        return Check::new(
            "allowlist (user)",
            Status::Skip,
            format!("absent ({})", path.display()),
        );
    }
    summarise_allowlist_file("allowlist (user)", &path)
}

fn check_project_allowlist(cwd: Option<&Path>) -> Check {
    let cwd = match cwd {
        Some(c) => c,
        None => {
            return Check::new(
                "allowlist (project)",
                Status::Warn,
                "cannot read current directory",
            );
        }
    };
    let root = match discover_project_root(cwd) {
        Some(r) => r,
        None => {
            return Check::new(
                "allowlist (project)",
                Status::Skip,
                "no .vet/allowlist.yaml found walking up from cwd",
            );
        }
    };
    let path = root.join(".vet").join("allowlist.yaml");
    summarise_allowlist_file("allowlist (project)", &path)
}

fn check_override_allowlist(path: &Path) -> Check {
    summarise_allowlist_file("allowlist (override)", path)
}

fn summarise_allowlist_file(name: &'static str, path: &Path) -> Check {
    match matcher::load_file(path) {
        Ok(file) => Check::new(name, Status::Ok, summarise_file(path, &file)),
        Err(e) => Check::new(name, Status::Error, render_load_error(&e)),
    }
}

fn summarise_file(path: &Path, file: &AllowlistFile) -> String {
    format!(
        "{} ({} rule{}, {} deny)",
        path.display(),
        file.rules.len(),
        plural(file.rules.len()),
        file.deny.len(),
    )
}

fn render_load_error(e: &LoadError) -> String {
    match e {
        LoadError::Io { path, source } => format!("read {}: {source}", path.display()),
        LoadError::Yaml { path, source } => format!("parse {}: {source}", path.display()),
        LoadError::Serialize { path, source } => format!("serialise {}: {source}", path.display()),
        LoadError::DuplicateId { id, path } => {
            format!("duplicate rule id `{id}` in {}", path.display())
        }
        LoadError::RuleNotFound { id, path } => {
            format!("no rule with id `{id}` in {}", path.display())
        }
    }
}

fn check_parsers() -> Check {
    let names = vetter_core::parsers::registered_names();
    let summary = if names.is_empty() {
        "none yet".to_string()
    } else {
        names.join(", ")
    };
    if names.is_empty() {
        Check::new(
            "parsers registered",
            Status::Error,
            "0 (parser registry is empty; binary likely misbuilt)",
        )
    } else {
        Check::new(
            "parsers registered",
            Status::Ok,
            format!("{}  ({summary})", names.len()),
        )
    }
}

fn check_code_signing() -> Check {
    Check::new(
        "code signing",
        Status::Skip,
        "not implemented (Phase 4 follow-up; see plans/Overview.md §11)",
    )
}

fn format_uptime(start: SystemTime) -> String {
    let dur = SystemTime::now().duration_since(start).unwrap_or_default();
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
