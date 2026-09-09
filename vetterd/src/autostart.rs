//! Login-item registration via Apple's [`SMAppService`] (macOS 13+).
//!
//! [`SMAppService`]: https://developer.apple.com/documentation/servicemanagement/smappservice
//!
//! Wraps the three calls that matter for our use case:
//!
//! - `+[SMAppService mainAppService]` — the singleton tied to the
//!   running app bundle.
//! - `-[SMAppService registerAndReturnError:]` — adds the bundle to
//!   the user's Login Items list.
//! - `-[SMAppService unregisterAndReturnError:]` — removes it.
//! - `-[SMAppService status]` — reads the current state, which the
//!   user can flip from outside our process via System Settings →
//!   General → Login Items.
//!
//! ## Bundle-location guard
//!
//! `SMAppService.mainApp.register` only does anything sensible when
//! the calling executable is inside a code-signed `.app` bundle —
//! launchd needs a `LaunchServices`-discoverable bundle to
//! re-launch on next login. We refuse to register otherwise (mirror
//! of [`crate::notifier::mac::verify_bundle_path`]) so dev runs of
//! `cargo run -p vetterd` and bare `target/release/vetterd` invocations
//! surface a clear error rather than silently registering a path
//! launchd will fail to open at next reboot.
//!
//! ## Reconciliation
//!
//! [`reconcile_with_settings`] is the recovery path called from
//! `vetterd::run` at startup: it reads
//! [`vetter_core::settings::Settings::autostart`] and the live
//! [`current`] state, then registers / unregisters only when the
//! two diverge. This means the user can disable autostart via System
//! Settings → Login Items (which we have no way to intercept) and
//! the next daemon launch obeys their decision instead of silently
//! re-registering. Conversely, a user who hand-edits
//! `~/.vet/settings.yaml` to flip `autostart: true` gets the
//! registration applied on the next launch without having to open
//! the popover.
//!
//! ## Cross-platform shape
//!
//! The whole module compiles on every target so `wire`, `lib.rs`, and
//! the CLI can call into it without `cfg` gates. Three backends sit
//! behind one API:
//!
//! - **macOS** — `SMAppService`, described above.
//! - **Linux** — an XDG autostart entry at
//!   `$XDG_CONFIG_HOME/autostart/vetter.desktop`. Autostart there is a
//!   file rather than an API (`plans/LinuxApp.md` §5.3), so `current`
//!   is a filesystem read and [`AutostartStatus::RequiresApproval`] is
//!   never returned.
//! - **Everything else** — a stub where [`current`] answers
//!   [`AutostartStatus::Unsupported`] and the mutators refuse.
//!
//! [`reconcile_with_settings`] no-ops only on `Unsupported`, so it
//! started converging real state on Linux the moment that backend
//! landed, with no change of its own.

// `AutostartStatus` lives in `vetter_core::settings` so the
// admin-socket wire module can reference it without pulling in the
// daemon crate. Re-exported here for the historic call sites
// (popover, doctor, CLI).
pub use vetter_core::settings::AutostartStatus;

#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// The running executable is not under an `.app` bundle's
    /// `Contents/MacOS/` directory; registration cannot proceed.
    #[error(
        "autostart requires running inside a code-signed .app bundle \
         (current executable: {exe}); launch via `open Vetter.app`"
    )]
    NotABundle { exe: String },
    /// `SMAppService` is the wrong tool for the job: macOS < 13,
    /// `ServiceManagement.framework` failed to load, or the build
    /// is targeting a non-macOS platform.
    #[error("autostart unavailable on this platform: {0}")]
    Unavailable(String),
    /// `[SMAppService register]` or `[unregister]` returned an
    /// `NSError`. The wrapped message is `error.localizedDescription`
    /// formatted into a Rust string.
    #[error("SMAppService {op} failed: {message}")]
    Apple { op: &'static str, message: String },
    /// Writing or removing the XDG autostart entry failed. Linux-only:
    /// there autostart *is* a file, so filesystem errors are a real
    /// failure mode that `SMAppService` never had (§5.3).
    #[error("autostart entry {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

// ── macOS implementation ────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod sys {
    use objc2::class;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSError;

    use super::{AutostartError, AutostartStatus};

    // ARC-equivalent retain/release; defined for the few call
    // sites that hand-spell objc semantics.

    // Ensure the linker pulls in the framework that hosts SMAppService
    // and the related kSM* error codes. Without this attribute the
    // dynamic `class!(SMAppService)` lookup may still succeed if
    // another crate has already pulled the framework in, but we
    // don't want to rely on that transitive dependency — keep the
    // link declaration explicit at the call site.
    #[link(name = "ServiceManagement", kind = "framework")]
    extern "C" {}

    pub fn current() -> AutostartStatus {
        let Some(service) = main_app_service() else {
            return AutostartStatus::Unsupported;
        };
        let status: i64 = unsafe { objc2::msg_send![&*service, status] };
        from_raw_status(status)
    }

    pub fn enable() -> Result<(), AutostartError> {
        super::enforce_bundle_guard()?;
        let service = main_app_service().ok_or_else(|| {
            AutostartError::Unavailable("SMAppService class not loadable on this system".into())
        })?;
        perform(&service, "register")
    }

    pub fn disable() -> Result<(), AutostartError> {
        super::enforce_bundle_guard()?;
        let service = main_app_service().ok_or_else(|| {
            AutostartError::Unavailable("SMAppService class not loadable on this system".into())
        })?;
        perform(&service, "unregister")
    }

    /// Convert from the raw `SMAppServiceStatus` integer Apple
    /// returns. Unknown values surface as `NotRegistered` (the
    /// safest fallback — we don't claim something is enabled when
    /// we don't actually know).
    fn from_raw_status(raw: i64) -> AutostartStatus {
        match raw {
            0 => AutostartStatus::NotRegistered,
            1 => AutostartStatus::Enabled,
            2 => AutostartStatus::RequiresApproval,
            3 => AutostartStatus::NotFound,
            _ => AutostartStatus::NotRegistered,
        }
    }

    /// Resolve `+[SMAppService mainAppService]`. Returns `None` if
    /// the class is not registered with the Objective-C runtime.
    fn main_app_service() -> Option<Retained<AnyObject>> {
        let cls = class!(SMAppService);
        let obj: *mut AnyObject = unsafe { objc2::msg_send![cls, mainAppService] };
        if obj.is_null() {
            return None;
        }
        // `mainAppService` returns a `+0` autoreleased reference per
        // ARC convention for class methods that are not named
        // `new`/`alloc`/`copy`/`mutableCopy`. `Retained::retain`
        // takes a strong reference so the drop releases.
        unsafe { Retained::retain(obj) }
    }

    /// Send `register` / `unregister` to the SMAppService instance
    /// and turn an `NSError` out-pointer into [`AutostartError`].
    fn perform(service: &AnyObject, op: &'static str) -> Result<(), AutostartError> {
        let mut err: *mut NSError = std::ptr::null_mut();
        let ok: bool = match op {
            "register" => unsafe { objc2::msg_send![service, registerAndReturnError: &mut err] },
            "unregister" => unsafe {
                objc2::msg_send![service, unregisterAndReturnError: &mut err]
            },
            _ => unreachable!("perform called with unknown op `{op}`"),
        };
        if ok {
            return Ok(());
        }
        let message = if err.is_null() {
            format!("{op} returned NO with no NSError attached")
        } else {
            // SAFETY: per Cocoa convention an out-pointer NSError
            // returned alongside a NO/false from a method without
            // `new`/`alloc`/`copy`/`mutableCopy` is autoreleased
            // (+0). `Retained::retain` ups the refcount; drop will
            // release. (`from_raw` would assume +1 and over-release
            // on drop.)
            let nserr: Retained<NSError> = unsafe { Retained::retain(err) }
                .expect("non-null err pointer should retain to a Retained<NSError>");
            nserr.localizedDescription().to_string()
        };
        Err(AutostartError::Apple { op, message })
    }
}

// ── Linux implementation (XDG autostart) ────────────────────────────

/// Linux autostart is a *file*, not an API (`plans/LinuxApp.md` §5.3):
/// the session manager runs every `.desktop` entry it finds under
/// `$XDG_CONFIG_HOME/autostart/` at login. So `enable` writes an
/// entry, `disable` removes it, and `current` reads the filesystem.
///
/// Three consequences fall out of that, all of which shape the code
/// below:
///
/// - `current()` is a stat plus a small parse rather than an IPC
///   round-trip, so the "cheap enough to call on every window open"
///   property macOS relies on still holds.
/// - [`AutostartStatus::RequiresApproval`] has no analogue — nothing
///   gates writing a file in your own config directory — and is never
///   returned here.
/// - The entry pins an **absolute** `Exec=`, so it silently stops
///   working if the binary moves. That is the one place this is worse
///   than `SMAppService`, which resolves a bundle through Launch
///   Services. [`current`] therefore stats the recorded `Exec` target
///   and reports [`AutostartStatus::NotFound`] when it has gone, which
///   is exactly the actionable state `vet doctor` wants to surface.
#[cfg(target_os = "linux")]
mod sys {
    use std::path::{Path, PathBuf};

    use vetter_core::fs_secure::{create_dir_secure, persist_at_mode};

    use super::{AutostartError, AutostartStatus};

    /// Mode for the entry itself. Deliberately `0644` rather than the
    /// `0600` used for `settings.yaml` and the audit log: the file
    /// holds no secrets — just a path that is already visible in
    /// `/proc` to anyone who can see the process — and `.desktop`
    /// entries are conventionally world-readable. The containing
    /// directory is created `0700` regardless, so nothing new is
    /// exposed to other UIDs.
    const ENTRY_MODE: u32 = 0o644;

    pub fn current() -> AutostartStatus {
        match vetter_core::paths::user_autostart_path() {
            Ok(path) => current_at(&path),
            // No `$HOME` and no `$XDG_CONFIG_HOME`: we cannot even
            // name the file, let alone read it. That is a genuinely
            // unsupported environment rather than "disabled".
            Err(_) => AutostartStatus::Unsupported,
        }
    }

    pub fn enable() -> Result<(), AutostartError> {
        let exe =
            std::env::current_exe().map_err(|e| AutostartError::Unavailable(e.to_string()))?;
        let path = entry_path()?;
        write_entry_at(&path, &exe)
    }

    pub fn disable() -> Result<(), AutostartError> {
        let path = entry_path()?;
        remove_entry_at(&path)
    }

    fn entry_path() -> Result<PathBuf, AutostartError> {
        vetter_core::paths::user_autostart_path()
            .map_err(|e| AutostartError::Unavailable(e.to_string()))
    }

    /// Read the entry at `path` and classify it.
    ///
    /// A read error maps to `NotRegistered` rather than to an error
    /// state, for the same reason the macOS driver falls back that
    /// way on an unknown status code: never claim autostart is *on*
    /// when we could not actually determine it.
    pub(crate) fn current_at(path: &Path) -> AutostartStatus {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return AutostartStatus::NotRegistered;
        };
        if is_disabled(&contents) {
            return AutostartStatus::NotRegistered;
        }
        match exec_target(&contents) {
            // An entry with no usable `Exec=` would never launch
            // anything. Report it as `NotFound` rather than
            // `Enabled`: the row should push the user to re-run
            // `enable` rather than reassure them.
            None => AutostartStatus::NotFound,
            Some(target) => {
                // Only an absolute path can be checked here. A bare
                // command name is resolved against `$PATH` by the
                // session at login, which we cannot reproduce, so we
                // decline to call it missing.
                if target.is_absolute() && !target.exists() {
                    AutostartStatus::NotFound
                } else {
                    AutostartStatus::Enabled
                }
            }
        }
    }

    /// Whether the entry is present but switched off.
    ///
    /// Two spellings are honoured because two ecosystems write them:
    /// `Hidden=true` is the XDG spec's "the user deleted this entry",
    /// and `X-GNOME-Autostart-enabled=false` is what GNOME Tweaks
    /// writes when you untick an application. A user who disabled us
    /// through their desktop's own UI must not have the daemon report
    /// `Enabled` — and, via `reconcile_with_settings`, silently
    /// re-enable it on the next launch.
    pub(crate) fn is_disabled(contents: &str) -> bool {
        field(contents, "Hidden").is_some_and(|v| v.eq_ignore_ascii_case("true"))
            || field(contents, "X-GNOME-Autostart-enabled")
                .is_some_and(|v| v.eq_ignore_ascii_case("false"))
    }

    /// The binary an entry would launch, if we can determine it.
    pub(crate) fn exec_target(contents: &str) -> Option<PathBuf> {
        let raw = field(contents, "Exec")?;
        let first = first_argument(raw)?;
        if first.is_empty() {
            return None;
        }
        Some(PathBuf::from(first))
    }

    /// Value of `Key=` in the entry, trimmed.
    ///
    /// Deliberately naive about groups: our entries have exactly one
    /// (`[Desktop Entry]`), and a parser that tracked group headers
    /// would be more code defending against a file we wrote
    /// ourselves. Comment lines are skipped so a `#Hidden=true` note
    /// is not read as a setting.
    fn field<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
        contents.lines().find_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim())
        })
    }

    /// First argument of an `Exec=` value, unquoted.
    ///
    /// The desktop-entry spec allows the program to be double-quoted
    /// with backslash escapes, which is how [`render_entry`] writes a
    /// path containing spaces. Anything else is taken up to the first
    /// space. Field codes (`%f`, `%U`) never appear in our own entry
    /// and are irrelevant to the first token.
    fn first_argument(raw: &str) -> Option<String> {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => out.push(chars.next()?),
                    '"' => return Some(out),
                    _ => out.push(c),
                }
            }
            // Unterminated quote: malformed, and we should not guess.
            None
        } else {
            Some(raw.split_whitespace().next()?.to_string())
        }
    }

    /// Render the autostart entry for `exec`.
    ///
    /// `Exec` is always quoted and escaped so a path containing a
    /// space or a quote round-trips through [`exec_target`] instead of
    /// producing an entry that launches the wrong thing.
    pub(crate) fn render_entry(exec: &Path) -> String {
        let quoted = quote_exec(&exec.display().to_string());
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Vetter\n\
             Comment=Approve or reject commands an AI agent wants to run\n\
             Exec={quoted}\n\
             Icon=dev.vetter.daemon\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n\
             # Written by `vet daemon autostart enable`. The Exec path is\n\
             # absolute and pinned at that moment: if the binary moves,\n\
             # `vet doctor` reports the autostart row as `not found`.\n"
        )
    }

    /// Double-quote and escape a value per the desktop-entry spec's
    /// quoting rules (backslash, double quote, backtick, dollar).
    fn quote_exec(value: &str) -> String {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for c in value.chars() {
            if matches!(c, '\\' | '"' | '`' | '$') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }

    /// Write the entry atomically, creating `~/.config/autostart` if
    /// needed. Same temp-then-rename discipline as the settings and
    /// allowlist writers, so a kill mid-write leaves the previous
    /// entry intact rather than a truncated one the session manager
    /// would choke on at next login.
    pub(crate) fn write_entry_at(path: &Path, exec: &Path) -> Result<(), AutostartError> {
        let io_err = |p: &Path| {
            let p = p.display().to_string();
            move |source: std::io::Error| AutostartError::Io {
                path: p.clone(),
                source,
            }
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_dir_secure(parent, 0o700).map_err(io_err(parent))?;
        }
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let tmp = match parent {
            Some(p) => tempfile::NamedTempFile::new_in(p),
            None => tempfile::NamedTempFile::new_in("."),
        }
        .map_err(io_err(path))?;
        {
            use std::io::Write as _;
            let mut handle = tmp.as_file();
            handle
                .write_all(render_entry(exec).as_bytes())
                .map_err(io_err(path))?;
            handle.flush().map_err(io_err(path))?;
        }
        persist_at_mode(tmp, path, ENTRY_MODE).map_err(io_err(path))
    }

    /// Remove the entry. Idempotent: a missing file is success, which
    /// keeps `disable` symmetric with Apple's no-op `unregister` and
    /// lets `reconcile_with_settings` call it without checking first.
    pub(crate) fn remove_entry_at(path: &Path) -> Result<(), AutostartError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AutostartError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }
}

// ── stub for every other target ─────────────────────────────────────

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod sys {
    use super::{AutostartError, AutostartStatus};

    pub fn current() -> AutostartStatus {
        AutostartStatus::Unsupported
    }

    pub fn enable() -> Result<(), AutostartError> {
        Err(AutostartError::Unavailable(
            "autostart supported on macOS and Linux targets only".into(),
        ))
    }

    pub fn disable() -> Result<(), AutostartError> {
        Err(AutostartError::Unavailable(
            "autostart supported on macOS and Linux targets only".into(),
        ))
    }
}

// ── public API (delegates to the right `sys` module) ────────────────

/// Read the current `[SMAppService.mainApp status]`. Cheap (one
/// Objective-C msg_send roundtrip, no IO); safe to call on every
/// popover open + every `vet daemon autostart status`. On non-macOS
/// targets returns [`AutostartStatus::Unsupported`].
pub fn current() -> AutostartStatus {
    sys::current()
}

/// Register the running bundle as a Login Item. Idempotent — calling
/// this when already enabled is a no-op (Apple's API returns
/// success).
///
/// Refuses to proceed if the running executable is not located
/// inside an `.app` bundle (see module docs for why). Always errors
/// on non-macOS targets.
pub fn enable() -> Result<(), AutostartError> {
    sys::enable()
}

/// Unregister the running bundle from Login Items. Idempotent —
/// safe to call even when never registered (Apple's API treats it
/// as a no-op).
///
/// The bundle-location guard still applies: there's no point sending
/// `unregister` from a non-bundle context, and refusing keeps the
/// API symmetric with [`enable`].
pub fn disable() -> Result<(), AutostartError> {
    sys::disable()
}

/// Apply `desired` to the OS state, but only when the two disagree.
/// Used by `vetterd::run` to converge the OS-level Login Item state
/// with the user's persisted preference each time the daemon starts.
///
/// Returns `Ok(true)` when a state change actually happened (so
/// callers can log it), `Ok(false)` when the OS already matched the
/// preference. Errors propagate unchanged.
pub fn reconcile_with_settings(desired_autostart: bool) -> Result<bool, AutostartError> {
    let now = current();
    if matches!(now, AutostartStatus::Unsupported) {
        // Nothing to converge to on non-macOS / non-bundle hosts.
        // Don't surface an error: settings.autostart=true on a
        // non-bundle dev path should not fail daemon startup.
        return Ok(false);
    }
    match (desired_autostart, now.is_enabled()) {
        (true, true) | (false, false) => Ok(false),
        (true, false) => {
            enable()?;
            Ok(true)
        }
        (false, true) => {
            disable()?;
            Ok(true)
        }
    }
}

/// Refuse to register / unregister unless we're inside an `.app`
/// bundle. Used by the macOS implementation; lifted to the parent
/// module so [`AutostartError::NotABundle`] construction lives next
/// to its docs.
#[cfg(target_os = "macos")]
fn enforce_bundle_guard() -> Result<(), AutostartError> {
    let exe = std::env::current_exe().map_err(|e| AutostartError::Unavailable(e.to_string()))?;
    if crate::notifier::mac::is_app_bundle_executable(&exe) {
        return Ok(());
    }
    Err(AutostartError::NotABundle {
        exe: exe.display().to_string(),
    })
}

#[cfg(test)]
#[path = "tests/autostart.rs"]
mod tests;
