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
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::process::ExitCode;
use std::time::SystemTime;

use vetter_core::known_hosts::{self, user_known_hosts_path, KnownHostsError, KnownHostsFile};
use vetter_core::matcher::{
    self, discover_project_root, user_allowlist_path, AllowlistFile, LoadError,
};
use vetter_core::peer_cred::{current_euid, peer_pid, peer_uid};
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
        check_vetter_dir(),
    ];
    checks.extend(check_allowlists(cwd.as_deref(), allowlist_override));
    checks.extend(check_known_hosts(cwd.as_deref()));
    checks.push(check_parsers());
    checks.extend(check_code_signing());
    checks.push(check_autostart());

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

    if peer != expected {
        drop(stream);
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

    // PID attestation: the kernel-reported peer pid must equal the
    // pidfile's POSIX-write-lock holder pid. A same-UID racer that
    // bound the socket without `fcntl`-locking the pidfile gets
    // caught here even though the UID check above passed. See
    // `plans/ThreatModel.md` T1 sequencing #1.
    let peer_pid_value = match peer_pid(&stream) {
        Ok(p) => p,
        Err(e) => {
            return Check::new(
                "daemon",
                Status::Error,
                format!(
                    "cannot read peer pid on {} ({e}); refusing to declare the daemon healthy",
                    socket_path.display()
                ),
            );
        }
    };
    drop(stream);

    let lock_pid = match pidfile::read_locker_pid(pidfile_path) {
        Ok(p) => p,
        Err(e) => {
            return Check::new(
                "daemon",
                Status::Error,
                format!(
                    "cannot probe pidfile lock at {} ({e}); \
                     refusing to declare the daemon healthy",
                    pidfile_path.display()
                ),
            );
        }
    };
    if lock_pid != Some(peer_pid_value) {
        let lock_label = match lock_pid {
            Some(p) => format!("pid {p}"),
            None => "no holder (pidfile is not locked)".to_string(),
        };
        return Check::new(
            "daemon",
            Status::Error,
            format!(
                "PID attestation failed: socket peer is pid {peer_pid_value}, \
                 but pidfile {} is locked by {lock_label}. \
                 Stop the running process at the socket path and restart with `vet daemon start`.",
                pidfile_path.display()
            ),
        );
    }

    Check::new(
        "daemon",
        Status::Ok,
        format!(
            "pid={}, uptime={}, peer_uid={peer}, lock_pid={peer_pid_value}",
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
///
/// Hardening §H1 / `plans/ThreatModel.md` §T8: after the writability
/// probe we also overlay file-mode and parent-dir-mode warnings.
/// `AuditLog::open` creates fresh files at `0600` and the parent dir
/// at `0700`; this row downgrades to `WARN` when either landed wider
/// (e.g. a pre-existing log from a previous version, or a user
/// running `vim` on the file with a permissive umask). The repair
/// hint goes in the detail string.
fn check_audit_log(audit_path: Result<&PathBuf, &vetter_core::paths::PathError>) -> Check {
    let path = match audit_path {
        Ok(p) => p,
        Err(e) => {
            return Check::new("audit log", Status::Error, format!("{e}"));
        }
    };
    // Mirror `AuditLog::open`'s `mode(0o600)` so that `vet doctor`
    // doesn't itself create a wide-mode file on a fresh install (the
    // probe would otherwise inherit the umask default and a follow-up
    // doctor run would WARN on the file we just created).
    if let Err(e) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        return Check::new(
            "audit log",
            Status::Error,
            format!("{}: {e}", path.display()),
        );
    }

    // Writability passed. Overlay perm warnings: file should be
    // 0600 and parent dir should be 0700 (per `vetterd::audit::
    // AuditLog::open`'s `OpenOptions::mode` + `create_dir_secure`).
    let mut warnings: Vec<String> = Vec::new();
    if let Some(msg) = wide_file_warning(path, 0o600) {
        warnings.push(msg);
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Some(msg) = wide_dir_warning(parent, 0o700) {
            warnings.push(msg);
        }
    }
    if warnings.is_empty() {
        Check::new(
            "audit log",
            Status::Ok,
            format!("{} (writable, mode 0600)", path.display()),
        )
    } else {
        Check::new(
            "audit log",
            Status::Warn,
            format!("{} (writable; {})", path.display(), warnings.join("; ")),
        )
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
        Ok(file) => {
            let summary = summarise_file(path, &file);
            // Hardening §H1 / ThreatModel §T8: fresh writes land at
            // 0600 (see `vetter_core::matcher::loader::write_file`).
            // Pre-existing files at 0644 leak the user's trust
            // surface; downgrade to WARN with a chmod hint.
            match wide_file_warning(path, 0o600) {
                Some(msg) => Check::new(name, Status::Warn, format!("{summary} ({msg})")),
                None => Check::new(name, Status::Ok, summary),
            }
        }
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

/// Mirror of [`check_allowlists`] for the known-hosts files. Always
/// emit the user + project rows so a user with a project file but no
/// user file (or vice versa) still gets coverage on whichever side
/// they own.
fn check_known_hosts(cwd: Option<&Path>) -> Vec<Check> {
    vec![check_user_known_hosts(), check_project_known_hosts(cwd)]
}

fn check_user_known_hosts() -> Check {
    let path = match user_known_hosts_path() {
        Some(p) => p,
        None => {
            return Check::new(
                "known-hosts (user)",
                Status::Warn,
                "$HOME is not set; cannot resolve user known-hosts path",
            );
        }
    };
    if !path.exists() {
        return Check::new(
            "known-hosts (user)",
            Status::Skip,
            format!("absent ({})", path.display()),
        );
    }
    summarise_known_hosts_file("known-hosts (user)", &path)
}

fn check_project_known_hosts(cwd: Option<&Path>) -> Check {
    let cwd = match cwd {
        Some(c) => c,
        None => {
            return Check::new(
                "known-hosts (project)",
                Status::Warn,
                "cannot read current directory",
            );
        }
    };
    let root = match discover_project_root(cwd) {
        Some(r) => r,
        None => {
            return Check::new(
                "known-hosts (project)",
                Status::Skip,
                "no .vet/known-hosts.yaml found walking up from cwd",
            );
        }
    };
    let path = root.join(".vet").join("known-hosts.yaml");
    if !path.exists() {
        return Check::new(
            "known-hosts (project)",
            Status::Skip,
            format!("absent ({})", path.display()),
        );
    }
    summarise_known_hosts_file("known-hosts (project)", &path)
}

fn summarise_known_hosts_file(name: &'static str, path: &Path) -> Check {
    match known_hosts::load_file(path) {
        Ok(file) => {
            let summary = summarise_known_hosts(path, &file);
            // Same H1 / T8 reasoning as the allowlist row: WARN when
            // an existing file is wider than 0600.
            match wide_file_warning(path, 0o600) {
                Some(msg) => Check::new(name, Status::Warn, format!("{summary} ({msg})")),
                None => Check::new(name, Status::Ok, summary),
            }
        }
        Err(e) => Check::new(name, Status::Error, render_known_hosts_error(&e)),
    }
}

fn summarise_known_hosts(path: &Path, file: &KnownHostsFile) -> String {
    format!(
        "{} ({} host{})",
        path.display(),
        file.hosts.len(),
        plural(file.hosts.len()),
    )
}

fn render_known_hosts_error(e: &KnownHostsError) -> String {
    match e {
        KnownHostsError::Io { path, source } => format!("read {}: {source}", path.display()),
        KnownHostsError::Yaml { path, source } => format!("parse {}: {source}", path.display()),
        KnownHostsError::Serialize { path, source } => {
            format!("serialise {}: {source}", path.display())
        }
        KnownHostsError::DuplicatePattern { pattern, path } => {
            format!(
                "duplicate known-host pattern `{pattern}` in {}",
                path.display()
            )
        }
    }
}

/// Validate `~/.vet/`: the shared parent of `allowlist.yaml`,
/// `known-hosts.yaml`, and `settings.yaml`. Mode `0700` is what
/// every vetter writer establishes via
/// [`vetter_core::fs_secure::create_dir_secure`]. ERROR on a
/// foreign-owned dir; WARN when a same-uid dir is wider than `0700`;
/// INFO when it doesn't exist yet (a fresh install before any
/// `vet allow add` / popover write).
fn check_vetter_dir() -> Check {
    let label = "vetter dir";
    let home = match std::env::var_os("HOME") {
        Some(h) => h,
        None => {
            return Check::new(label, Status::Warn, "$HOME is not set");
        }
    };
    let path = PathBuf::from(home).join(".vet");
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Check::new(
                label,
                Status::Info,
                format!(
                    "absent ({}); will be created with mode 0700 on next write",
                    path.display()
                ),
            );
        }
        Err(e) => {
            return Check::new(
                label,
                Status::Error,
                format!("stat {}: {e}", path.display()),
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
    let detail = format!("{} (mode 0{mode:o}, owner {owner_label})", path.display());
    if owner != self_uid {
        return Check::new(
            label,
            Status::Error,
            format!("{detail} (expected owner self)"),
        );
    }
    if mode > 0o700 {
        return Check::new(
            label,
            Status::Warn,
            format!(
                "{detail} (expected mode 0700; chmod 0700 {})",
                path.display()
            ),
        );
    }
    Check::new(label, Status::Ok, detail)
}

/// If `path` exists and its on-disk mode is wider than `expected`,
/// or it is owned by another uid, return a single-line warning
/// suitable for overlaying onto an existing row's detail. Returns
/// `None` when the file is absent (caller already handled that) or
/// the permissions are tight enough.
///
/// We compare with `>` rather than `!=` so a *tighter* mode (e.g.
/// `0400` on the audit log) is treated as fine, not a regression.
fn wide_file_warning(path: &Path, expected: u32) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    let owner = meta.uid();
    let self_uid = current_euid();
    if owner != self_uid {
        return Some(format!("owner uid={owner}, expected self ({self_uid})"));
    }
    if mode > expected {
        return Some(format!(
            "mode 0{mode:o}, expected 0{expected:o}; chmod 0{expected:o} {}",
            path.display()
        ));
    }
    None
}

/// Same shape as [`wide_file_warning`] but for a directory's mode.
/// Used by the audit log row to flag a wide parent dir; the standalone
/// `vetter dir` and `socket parent dir` rows use bespoke logic so they
/// can carry their own status (ERROR on foreign owner) rather than
/// just an overlay string.
fn wide_dir_warning(path: &Path, expected: u32) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    let owner = meta.uid();
    let self_uid = current_euid();
    if owner != self_uid {
        return Some(format!(
            "parent {} owned by uid={owner}, expected self ({self_uid})",
            path.display()
        ));
    }
    if mode > expected {
        return Some(format!(
            "parent {} mode 0{mode:o}, expected 0{expected:o}; chmod 0{expected:o} {}",
            path.display(),
            path.display()
        ));
    }
    None
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

/// Code-signing rows. On non-macOS this is a single SKIP — the
/// distribution channel is macOS-only (`plans/Overview.md` §12), so a
/// signed binary on Linux is meaningless. On macOS we emit one row per
/// inspected artifact: the running `vet`, the resolved `vetterd`, and
/// (when both binaries live inside the same `.app/Contents/MacOS/`)
/// the enclosing bundle's notarisation + staple state.
///
/// Verdict matrix per
/// [plans/TestingPlan.md](../../plans/TestingPlan.md) §4.7:
///
/// - Per-binary: `OK` for Developer-ID + hardened runtime, `WARN` for
///   ad-hoc signed (locally built), `ERROR` if `codesign --verify`
///   fails or the binary path cannot be resolved.
/// - Bundle: `OK` for Developer-ID + notarised + stapled, `WARN`
///   otherwise, `ERROR` if `codesign --verify` against the bundle
///   fails. The row is omitted when the binaries are not inside an
///   `.app` (typical cargo-build path).
fn check_code_signing() -> Vec<Check> {
    #[cfg(not(target_os = "macos"))]
    {
        vec![Check::new("code signing", Status::Skip, "macOS only")]
    }
    #[cfg(target_os = "macos")]
    {
        let vet_path = std::env::current_exe().ok();
        let vetterd_path = crate::daemon::locate_vetterd().ok();
        let mut checks = vec![
            check_binary_signature("code signing (vet)", vet_path.as_deref()),
            check_binary_signature("code signing (vetterd)", vetterd_path.as_deref()),
        ];
        if let Some(bundle) = shared_bundle_root(vet_path.as_deref(), vetterd_path.as_deref()) {
            checks.push(check_bundle_signature(&bundle));
        }
        checks
    }
}

/// On macOS only. Walks the parents of `exe` looking for a
/// `<Name>.app/Contents/MacOS/<exe>` ancestor and returns the `.app`
/// path. Mirrors the reachability check in
/// `vetterd::notifier::mac::is_app_bundle_executable`; duplicated
/// rather than shared because the two crates already keep the helper
/// small and the dependency would only be used for this row.
#[cfg(target_os = "macos")]
fn bundle_root_for(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name().and_then(|s| s.to_str()) != Some("MacOS") {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name().and_then(|s| s.to_str()) != Some("Contents") {
        return None;
    }
    let app_dir = contents_dir.parent()?;
    if !app_dir
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name.ends_with(".app"))
    {
        return None;
    }
    Some(app_dir.to_path_buf())
}

/// Returns the `.app` path iff both binaries resolve to executables
/// living inside the *same* bundle. A mismatch (e.g. `vet` from a
/// bundle but `vetterd` resolved via PATH to `/usr/local/bin/vetterd`)
/// means the bundle row would be ambiguous; we omit it instead.
#[cfg(target_os = "macos")]
fn shared_bundle_root(vet: Option<&Path>, vetterd: Option<&Path>) -> Option<PathBuf> {
    let vet = vet?;
    let vetterd = vetterd?;
    let vet_bundle = bundle_root_for(vet)?;
    let vetterd_bundle = bundle_root_for(vetterd)?;
    if vet_bundle == vetterd_bundle {
        Some(vet_bundle)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Default, PartialEq, Eq)]
struct CodesignInfo {
    /// `Signature=adhoc` line present.
    adhoc: bool,
    /// First `Authority=Developer ID Application: …` line, verbatim
    /// (without the `Authority=` prefix). `None` when the leaf
    /// authority is something else (e.g. Apple-internal "Software
    /// Signing"), or when the binary is ad-hoc / unsigned.
    developer_id: Option<String>,
    /// `TeamIdentifier=` value when present and not literally
    /// `not set`.
    team_id: Option<String>,
    /// True iff the `flags=…` bitset on the `CodeDirectory` line
    /// includes `runtime` — the `--options runtime` codesign flag.
    hardened_runtime: bool,
}

/// Parse the metadata block `codesign -d -vv` writes to stderr. The
/// parser is line-oriented and tolerant of unrecognised fields; new
/// codesign versions add fields at the end of the block (e.g.
/// `CMSDigest`) which we deliberately ignore.
#[cfg(target_os = "macos")]
fn parse_codesign_display(text: &str) -> CodesignInfo {
    let mut info = CodesignInfo::default();
    for line in text.lines() {
        let line = line.trim();
        if line == "Signature=adhoc" {
            info.adhoc = true;
        } else if let Some(rest) = line.strip_prefix("Authority=") {
            if rest.starts_with("Developer ID Application:") && info.developer_id.is_none() {
                info.developer_id = Some(rest.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("TeamIdentifier=") {
            if rest != "not set" {
                info.team_id = Some(rest.to_string());
            }
        } else if line.starts_with("CodeDirectory") && line.contains("(") {
            // flags appear as `flags=0xNNNN(label1,label2,…)`; the
            // label list contains `runtime` when the hardened runtime
            // option was set at sign time. Substring match is enough
            // — the labels are a flat comma list.
            if let Some(open) = line.find('(') {
                if let Some(close) = line[open..].find(')') {
                    let labels = &line[open + 1..open + close];
                    if labels.split(',').any(|l| l.trim() == "runtime") {
                        info.hardened_runtime = true;
                    }
                }
            }
        }
    }
    info
}

#[cfg(target_os = "macos")]
fn check_binary_signature(label: &'static str, path: Option<&Path>) -> Check {
    let Some(path) = path else {
        return Check::new(
            label,
            Status::Error,
            "cannot resolve binary path \
             (set $VETTERD_BIN or place vetterd next to vet)",
        );
    };
    if !path.exists() {
        return Check::new(label, Status::Error, format!("missing: {}", path.display()));
    }
    match codesign_verdict(path) {
        Ok(check) => Check::new(label, check.0, check.1),
        Err(e) => Check::new(label, Status::Error, e),
    }
}

/// Returns the `(Status, detail)` pair for a single binary path, or
/// an error string describing why the verdict could not be computed.
/// Pulled out so [`check_binary_signature`] stays a thin shell that
/// turns the path-missing case into ERROR with a useful detail.
#[cfg(target_os = "macos")]
fn codesign_verdict(path: &Path) -> Result<(Status, String), String> {
    let verify = Command::new("codesign")
        .args(["--verify", "--strict"])
        .arg(path)
        .output()
        .map_err(|e| format!("spawning codesign --verify failed: {e}"))?;
    if !verify.status.success() {
        let stderr = String::from_utf8_lossy(&verify.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!(
                "codesign --verify --strict {} exited non-zero",
                path.display()
            )
        } else {
            format!("{}: {stderr}", path.display())
        };
        return Ok((Status::Error, detail));
    }

    // `codesign -d` writes its metadata block to stderr, not stdout.
    let display = Command::new("codesign")
        .args(["-d", "-vv"])
        .arg(path)
        .output()
        .map_err(|e| format!("spawning codesign -d -vv failed: {e}"))?;
    if !display.status.success() {
        let stderr = String::from_utf8_lossy(&display.stderr).trim().to_string();
        return Ok((
            Status::Error,
            format!("codesign -d {}: {stderr}", path.display()),
        ));
    }
    let info = parse_codesign_display(&String::from_utf8_lossy(&display.stderr));

    if info.adhoc {
        return Ok((
            Status::Warn,
            format!("{} (ad-hoc signed; locally built)", path.display()),
        ));
    }
    if let Some(authority) = info.developer_id {
        let mut bits: Vec<String> = Vec::new();
        bits.push(authority);
        if let Some(team) = info.team_id {
            bits.push(format!("Team {team}"));
        }
        if info.hardened_runtime {
            bits.push("hardened runtime".into());
        } else {
            // No hardened runtime → notary will reject, so this is
            // not a healthy distribution build even if everything
            // else looks Developer-ID-shaped.
            return Ok((
                Status::Warn,
                format!(
                    "{}: {} (no hardened runtime)",
                    path.display(),
                    bits.join(", ")
                ),
            ));
        }
        return Ok((
            Status::Ok,
            format!("{}: {}", path.display(), bits.join(", ")),
        ));
    }
    Ok((
        Status::Warn,
        format!(
            "{} (signature present but not Developer ID Application)",
            path.display()
        ),
    ))
}

/// Bundle-level check: `xcrun stapler validate` for the notarisation
/// staple, then `spctl --assess --type execute` for Gatekeeper
/// acceptance. OK only when both succeed; otherwise WARN with a hint
/// pointing at `tools/release.sh` (canonical Developer-ID +
/// notarisation pipeline).
#[cfg(target_os = "macos")]
fn check_bundle_signature(bundle: &Path) -> Check {
    let label = "code signing (bundle)";

    let verify = Command::new("codesign")
        .args(["--verify", "--strict"])
        .arg(bundle)
        .output();
    match verify {
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Check::new(
                label,
                Status::Error,
                format!("codesign --verify {}: {stderr}", bundle.display()),
            );
        }
        Err(e) => {
            return Check::new(
                label,
                Status::Error,
                format!("spawning codesign --verify failed: {e}"),
            );
        }
        Ok(_) => {}
    }

    let staple = Command::new("xcrun")
        .args(["stapler", "validate"])
        .arg(bundle)
        .output();
    let stapled = matches!(staple, Ok(out) if out.status.success());

    let assess = Command::new("spctl")
        .args(["--assess", "--type", "execute", "--verbose=4"])
        .arg(bundle)
        .output();
    let (accepted, source) = match assess {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let accepted = out.status.success();
            // spctl emits a `source=…` line on stderr when the
            // assessment found a signature — surface it so the
            // OK/WARN row tells the user which trust path matched.
            let source = combined
                .lines()
                .find_map(|l| l.trim().strip_prefix("source="))
                .map(|s| s.to_string());
            (accepted, source)
        }
        Err(_) => (false, None),
    };

    if stapled && accepted {
        let detail = match source {
            Some(s) => format!("{} (stapled, {s})", bundle.display()),
            None => format!("{} (stapled, accepted)", bundle.display()),
        };
        return Check::new(label, Status::Ok, detail);
    }

    let mut reasons: Vec<String> = Vec::new();
    if !stapled {
        reasons.push("no notarisation ticket stapled".into());
    }
    if !accepted {
        match source.as_deref() {
            Some(s) => reasons.push(format!("spctl rejected (source={s})")),
            None => reasons.push("spctl rejected".into()),
        }
    }
    Check::new(
        label,
        Status::Warn,
        format!(
            "{}: {} (run tools/release.sh for a Developer-ID + notarised + stapled bundle)",
            bundle.display(),
            reasons.join(", "),
        ),
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

/// Build the `autostart` row.
///
/// Reports whether `Vetter.app` is registered as a macOS Login
/// Item via `SMAppService.mainApp`. Talks to the daemon over the
/// admin socket so the row reflects the *live* OS status (which
/// the user can flip via System Settings -> Login Items between
/// vet invocations).
///
/// Falls back to reading `~/.vet/settings.yaml` directly when the
/// daemon is not reachable, so the user at least sees their
/// persisted preference.
fn check_autostart() -> Check {
    if !cfg!(target_os = "macos") {
        return Check::new("autostart", Status::Skip, "macOS only");
    }
    use vetter_core::settings::AutostartStatus;
    use vetter_core::wire::{MgmtRequest, MgmtResponse};

    match crate::daemon::query_admin(MgmtRequest::GetAutostart) {
        Ok(MgmtResponse::AutostartState { desired, status }) => match status {
            AutostartStatus::Enabled => {
                Check::new("autostart", Status::Ok, "enabled (login item registered)")
            }
            AutostartStatus::NotRegistered => {
                let detail = if desired {
                    "preference says enabled but OS reports not registered; \
                     try `vet daemon autostart enable`"
                        .to_string()
                } else {
                    "disabled".to_string()
                };
                let status = if desired { Status::Warn } else { Status::Info };
                Check::new("autostart", status, detail)
            }
            AutostartStatus::RequiresApproval => Check::new(
                "autostart",
                Status::Warn,
                "requires approval; open System Settings -> General -> Login Items",
            ),
            AutostartStatus::NotFound => Check::new(
                "autostart",
                Status::Warn,
                "bundle not registered (try `vet daemon autostart enable`)",
            ),
            AutostartStatus::Unsupported => Check::new(
                "autostart",
                Status::Skip,
                "needs macOS 13+ inside Vetter.app",
            ),
        },
        Ok(MgmtResponse::Error { message }) => Check::new(
            "autostart",
            Status::Warn,
            format!("daemon rejected query: {message}"),
        ),
        Ok(other) => Check::new(
            "autostart",
            Status::Warn,
            format!("unexpected daemon response: {other:?}"),
        ),
        Err(_) => {
            // Daemon down — fall back to reading the persisted
            // preference so the user still gets something useful.
            match vetter_core::settings::load() {
                Ok(s) if s.autostart => Check::new(
                    "autostart",
                    Status::Info,
                    "preference: enabled (daemon offline; OS state unverified)",
                ),
                Ok(_) => Check::new(
                    "autostart",
                    Status::Info,
                    "preference: disabled (daemon offline; OS state unverified)",
                ),
                Err(e) => Check::new(
                    "autostart",
                    Status::Warn,
                    format!("could not read ~/.vet/settings.yaml: {e}"),
                ),
            }
        }
    }
}
