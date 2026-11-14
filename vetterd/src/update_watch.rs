//! Daemon-side stale-build notice.
//!
//! A background thread compares the daemon's own version with the
//! latest release once a day and, when it is behind, raises one
//! banner per release through [`Notifier::notify_update`]. The banner's
//! Restart action calls [`restart_daemon`].
//!
//! The lookup itself is `vet update-refresh`, so the network client
//! and the brew/GitHub logic stay in the `vet` binary that sits beside
//! the daemon. The thread honours `check_for_updates` and, like `vet`,
//! stays out of the way when `$VETTERD_SOCKET` points at a daemon that
//! is not the user's installed one.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use vetter_core::update::{
    cache_path, load_cache_from, notice_text, parse_version, staleness, store_cache_to, Staleness,
    UpdateCache,
};

use crate::notifier::Notifier;

/// Time to let startup settle before the first lookup.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const SHUTDOWN_POLL: Duration = Duration::from_secs(1);

/// Start the watcher thread. It exits once `shutdown` is set.
pub fn spawn(notifier: Arc<dyn Notifier>, shutdown: Arc<AtomicBool>) {
    if std::env::var_os("VETTERD_SOCKET").is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("vetterd-update".into())
        .spawn(move || {
            let mut wait = FIRST_CHECK_DELAY;
            while sleep_unless_shutdown(wait, &shutdown) {
                if vetter_core::load_settings().is_ok_and(|s| s.check_for_updates) {
                    check_once(notifier.as_ref());
                }
                wait = CHECK_INTERVAL;
            }
        });
}

/// Sleep for `total`, waking early on shutdown. Returns `false` when
/// shutdown was requested.
fn sleep_unless_shutdown(total: Duration, shutdown: &AtomicBool) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if shutdown.load(Ordering::SeqCst) {
            return false;
        }
        let step = left.min(SHUTDOWN_POLL);
        std::thread::sleep(step);
        left -= step;
    }
    !shutdown.load(Ordering::SeqCst)
}

fn check_once(notifier: &dyn Notifier) {
    let Ok(path) = cache_path() else { return };
    let now = unix_now();
    if !load_cache_from(&path).is_some_and(|c| c.is_fresh(now)) {
        refresh_via_vet();
    }
    let Some(cache) = load_cache_from(&path) else {
        return;
    };
    let Ok(own) = parse_version(vetter_core::version()) else {
        return;
    };
    let Some(announcement) = announcement_for(&cache, &own) else {
        return;
    };
    notifier.notify_update(&announcement.notice);
    let _ = store_cache_to(
        &path,
        &UpdateCache {
            notified_version: Some(announcement.latest.to_string()),
            ..cache
        },
    );
}

/// What to tell the user about `cache`, if anything.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Announcement {
    pub notice: String,
    pub latest: Version,
}

/// A release is announced once: nothing when the daemon is current,
/// the latest release is unknown, or this release was already announced.
pub(crate) fn announcement_for(cache: &UpdateCache, own: &Version) -> Option<Announcement> {
    let latest = cache.latest_version().ok().flatten()?;
    if cache.notified_version.as_deref() == Some(latest.to_string().as_str()) {
        return None;
    }
    match staleness(Some(own), &latest) {
        Staleness::Current => None,
        Staleness::Behind { running, latest } => Some(Announcement {
            notice: notice_text(running.as_ref(), &latest),
            latest,
        }),
    }
}

fn refresh_via_vet() {
    if let Some(vet) = find_vet() {
        let _ = Command::new(vet)
            .arg("update-refresh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Replace this daemon with a freshly started one. Spawned detached,
/// because the first thing it does is stop us.
///
/// Inside a `.app` bundle the relaunch goes through `open`, so Launch
/// Services applies the bundle's environment and notification
/// identity; elsewhere `vet daemon restart` does both halves.
pub fn restart_daemon() {
    let Some(vet) = find_vet() else {
        eprintln!("vetterd: restart requested but no `vet` binary was found");
        return;
    };
    let bundle = std::env::current_exe()
        .ok()
        .and_then(|exe| enclosing_app_bundle(&exe));
    if let Err(e) = restart_command(&vet, bundle.as_deref())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        eprintln!("vetterd: restart failed to start: {e}");
    }
}

pub(crate) fn restart_command(vet: &Path, bundle: Option<&Path>) -> Command {
    match bundle {
        Some(bundle) => {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", r#""$1" daemon stop; exec open "$2""#, "sh"])
                .arg(vet)
                .arg(bundle);
            cmd
        }
        None => {
            let mut cmd = Command::new(vet);
            cmd.args(["daemon", "restart"]);
            cmd
        }
    }
}

pub(crate) fn enclosing_app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// The `vet` that ships beside this daemon, else the first on `PATH`.
fn find_vet() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("vet")))
        .filter(|p| p.is_file());
    beside.or_else(|| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join("vet"))
            .find(|p| p.is_file())
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
#[path = "tests/update_watch.rs"]
mod tests;
