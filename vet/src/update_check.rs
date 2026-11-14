//! Stale-daemon detection for the `vet` CLI.
//!
//! Compares the running daemon's version (admin `GetVersion`) with the
//! latest release: Homebrew's cask on macOS, GitHub Releases elsewhere
//! or when brew has nothing to say. The pure comparison and cache
//! live in [`vetter_core::update`]; this module does the fetching and
//! decides when a notice is worth printing.
//!
//! Interactive commands (`daemon status`, `doctor`, ...) may block on a
//! lookup. The wrap hot path never does: it refreshes in a detached
//! `vet update-refresh` process and reads whatever the cache holds.
//!
//! Checks are skipped when `$VETTERD_SOCKET` points somewhere other
//! than the user's own daemon (tests, side-by-side installs), unless
//! `$VET_UPDATE_LATEST` supplies the latest version directly.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use vetter_core::update::{
    cache_path, load_cache_from, notice_text, parse_brew_cask_version,
    parse_github_release_version, parse_version, staleness, store_cache_to, Staleness, UpdateCache,
};
use vetter_core::wire::{MgmtRequest, MgmtResponse};
use vetter_core::{default_socket_path, load_settings};

const CASK: &str = "blevinstein/vetter/vetter";
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/blevinstein/vetter/releases/latest";
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// What the daemon said when asked for its version.
pub enum DaemonVersion {
    NotRunning,
    /// Reachable but silent: it predates version reporting.
    Unanswered,
    Reported(Version),
}

/// Whether a missing or stale cache may be refreshed inline.
#[derive(Clone, Copy)]
pub enum Fetch {
    Blocking,
    Background,
}

pub fn daemon_version() -> DaemonVersion {
    if !crate::daemon::socket_alive(&default_socket_path()) {
        return DaemonVersion::NotRunning;
    }
    match crate::daemon::query_admin(MgmtRequest::GetVersion) {
        Ok(MgmtResponse::Version { version }) => match parse_version(&version) {
            Ok(v) => DaemonVersion::Reported(v),
            Err(_) => DaemonVersion::Unanswered,
        },
        _ => DaemonVersion::Unanswered,
    }
}

/// Compare the running daemon with the latest release. `None` when
/// checks are disabled or skipped, the latest version is not yet known,
/// or no daemon is running.
pub fn daemon_staleness(fetch: Fetch) -> Option<Staleness> {
    let latest = latest_release(fetch)?;
    let running = match daemon_version() {
        DaemonVersion::NotRunning => return None,
        DaemonVersion::Unanswered => None,
        DaemonVersion::Reported(v) => Some(v),
    };
    Some(staleness(running.as_ref(), &latest))
}

/// The warning for a stale daemon, or `None` when it is current or
/// cannot be assessed.
pub fn stale_notice(fetch: Fetch) -> Option<String> {
    match daemon_staleness(fetch)? {
        Staleness::Current => None,
        Staleness::Behind { running, latest } => Some(notice_text(running.as_ref(), &latest)),
    }
}

/// Wrap-path hook: at most once per day, check the daemon against the
/// cached latest release and warn on stderr. Never blocks on the network.
pub fn warn_if_stale() {
    let Ok(path) = cache_path() else { return };
    let now = unix_now();
    if load_cache_from(&path).is_some_and(|c| !c.daemon_check_due(now)) {
        return;
    }
    let Some(staleness) = daemon_staleness(Fetch::Background) else {
        return;
    };
    // Reload: the lookup above may have stamped the cache.
    if let Some(mut cache) = load_cache_from(&path) {
        cache.daemon_checked_at = Some(now);
        let _ = store_cache_to(&path, &cache);
    }
    if let Staleness::Behind { running, latest } = staleness {
        eprintln!("vet: warning: {}", notice_text(running.as_ref(), &latest));
    }
}

/// Entry point of the hidden `vet update-refresh` subcommand.
pub fn refresh() {
    if let Ok(path) = cache_path() {
        refresh_cache(&path, unix_now());
    }
}

fn latest_release(fetch: Fetch) -> Option<Version> {
    if let Some(forced) = std::env::var_os("VET_UPDATE_LATEST") {
        return parse_version(&forced.to_string_lossy()).ok();
    }
    if std::env::var_os("VETTERD_SOCKET").is_some()
        || !load_settings().is_ok_and(|s| s.check_for_updates)
    {
        return None;
    }
    let path = cache_path().ok()?;
    let now = unix_now();
    if !load_cache_from(&path).is_some_and(|c| c.is_fresh(now)) {
        match fetch {
            Fetch::Blocking => refresh_cache(&path, now),
            Fetch::Background => spawn_refresh(&path, now),
        }
    }
    load_cache_from(&path)?.latest_version().ok().flatten()
}

fn refresh_cache(path: &Path, now: u64) {
    let previous = load_cache_from(path);
    let latest = fetch_latest().ok().map(|v| v.to_string());
    let cache = UpdateCache {
        checked_at: now,
        latest: latest.or_else(|| previous.as_ref().and_then(|c| c.latest.clone())),
        daemon_checked_at: previous.and_then(|c| c.daemon_checked_at),
    };
    let _ = store_cache_to(path, &cache);
}

/// Stamp the attempt, then lookup in a detached child so concurrent
/// commands neither pile up lookups nor wait on one.
fn spawn_refresh(path: &Path, now: u64) {
    let previous = load_cache_from(path);
    let stamp = UpdateCache {
        checked_at: now,
        latest: previous.as_ref().and_then(|c| c.latest.clone()),
        daemon_checked_at: previous.and_then(|c| c.daemon_checked_at),
    };
    if store_cache_to(path, &stamp).is_err() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("update-refresh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let _ = cmd.spawn();
}

fn fetch_latest() -> Result<Version, String> {
    if cfg!(target_os = "macos") {
        if let Ok(v) = fetch_from_brew() {
            return Ok(v);
        }
    }
    fetch_from_github()
}

fn fetch_from_brew() -> Result<Version, String> {
    let out = Command::new("brew")
        .args(["info", "--cask", "--json=v2", CASK])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("brew exited with {}", out.status));
    }
    parse_brew_cask_version(&String::from_utf8_lossy(&out.stdout)).map_err(|e| e.to_string())
}

fn fetch_from_github() -> Result<Version, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .build()
        .into();
    let body = agent
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "vet-update-check")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_github_release_version(&body).map_err(|e| e.to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
