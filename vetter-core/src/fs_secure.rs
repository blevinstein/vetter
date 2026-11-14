//! Filesystem helpers that enforce restrictive Unix modes.
//!
//! Hardening §H1 / `plans/ThreatModel.md` §T8: every vetter-owned
//! file (audit log, allowlist YAML, known-hosts YAML, settings YAML)
//! must land at mode `0600`, and every parent directory we create
//! must land at mode `0700`. Without this we inherit the calling
//! user's umask (typically `022`) and the audit log — which carries
//! verbatim argv, i.e. routinely carries secrets — ends up
//! world-readable on disk.
//!
//! Two helpers cover every call site:
//!
//! - [`create_dir_secure`] — `create_dir_all` then chmod the leaf.
//!   We deliberately only chmod the leaf rather than every component
//!   walked: `~/.vet/` is ours to tighten, but `$HOME` is not, and
//!   `~/Library/Logs/` is shared by every Library-using app on macOS.
//! - [`persist_at_mode`] — chmod a `tempfile::NamedTempFile` *before*
//!   `persist`, so the rename produces an already-tight file rather
//!   than briefly exposing it at the umask default.
//!
//! Behaviour on existing files: `OpenOptions::mode` only applies
//! when the file is newly created. We deliberately do not chmod
//! files we didn't create on this run — surprising the user by
//! tightening modes on files they touched themselves is its own
//! footgun. The user-visible repair path is the
//! `vet doctor` perm-checks row, which surfaces a `WARN` and a
//! `chmod 0600 …` hint.

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

/// `create_dir_all(path)` then chmod the leaf to `mode`. Idempotent
/// — repeated calls on an existing directory simply re-apply the
/// permission bits.
pub fn create_dir_secure(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, Permissions::from_mode(mode))?;
    Ok(())
}

/// Set `tmp`'s permissions to `mode` and then atomically `persist`
/// it to `dest`. The permissions are set on the tempfile itself
/// before the rename so the destination never momentarily exists at
/// the umask default.
pub fn persist_at_mode(tmp: tempfile::NamedTempFile, dest: &Path, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(tmp.path(), Permissions::from_mode(mode))?;
    tmp.persist(dest).map_err(|e| e.error)?;
    Ok(())
}

/// Write `contents` to `dest` atomically at `mode`, creating the parent
/// directory (at `0700`) if needed. The bytes land in a sibling
/// tempfile that is fsynced and then renamed over `dest`, so readers
/// see either the old file or the whole new one.
pub fn write_atomic(dest: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write as _;
    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(p) = parent {
        create_dir_secure(p, 0o700)?;
    }
    let tmp = tempfile::NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new(".")))?;
    let mut handle = tmp.as_file();
    handle.write_all(contents)?;
    handle.sync_all()?;
    persist_at_mode(tmp, dest, mode)
}

#[cfg(test)]
#[path = "tests/fs_secure.rs"]
mod tests;
