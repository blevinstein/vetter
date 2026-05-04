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
//! the CLI can call into it without `cfg` gates. On non-macOS targets
//! [`current`] returns [`AutostartStatus::Unsupported`] and
//! [`enable`] / [`disable`] both return [`AutostartError::Unavailable`]
//! immediately. This matches the project's "Linux support is Phase 6"
//! posture without forcing every caller to branch on `target_os`.

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

// ── non-macOS stub ──────────────────────────────────────────────────

#[cfg(not(target_os = "macos"))]
mod sys {
    use super::{AutostartError, AutostartStatus};

    pub fn current() -> AutostartStatus {
        AutostartStatus::Unsupported
    }

    pub fn enable() -> Result<(), AutostartError> {
        Err(AutostartError::Unavailable(
            "autostart only supported on macOS targets".into(),
        ))
    }

    pub fn disable() -> Result<(), AutostartError> {
        Err(AutostartError::Unavailable(
            "autostart only supported on macOS targets".into(),
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
