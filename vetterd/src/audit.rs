//! JSON-lines audit log.
//!
//! Per `plans/Overview.md` §7 and `plans/TestingPlan.md` §5.4, every
//! decision (auto-allow, auto-deny, eventual human-allow, ...) is
//! appended as one JSON line. The file is `sync_data`-flushed before
//! the daemon writes the decision frame back to the client, so a
//! crash between "decision sent" and "next request" never loses an
//! audit entry.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vetter_core::wire::WireDecision;

/// One audit-log line. JSON-Lines (no trailing comma, terminated with
/// `\n`). Fields chosen per `plans/TestingPlan.md` §5.4 — `id`,
/// `timestamp`, `command`, `argv`, `decision`, `reason`, `rule_id`.
/// Phase 3a adds `force_prompt` so a `--dry-run`-driven entry is
/// distinguishable from a real prompt-class miss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub timestamp: String,
    pub command: String,
    pub argv: Vec<String>,
    pub decision: WireDecision,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub force_prompt: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Append-only JSON-lines log. Cheap to share across worker threads
/// thanks to the inner `Mutex<File>`. Locking is fine-grained
/// (one write per request) and the critical section is bounded by
/// `serialize → write → fsync`, all of which are sub-millisecond.
pub struct AuditLog {
    file: Mutex<File>,
    path: PathBuf,
}

impl AuditLog {
    /// Open (creating if missing) the log at `path`. Parent dirs are
    /// created — `~/Library/Logs/vetter/` may not yet exist on a
    /// fresh install.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_path_buf(),
        })
    }

    /// Append one entry, terminated with `\n`, then `sync_data` to
    /// flush the page cache. Returns IO errors verbatim — the daemon
    /// logs and continues rather than killing the connection, since
    /// "answer the request" is more important than "log the answer".
    pub fn append(&self, entry: &AuditEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("serialise: {e}"))
        })?;
        line.push(b'\n');
        let mut g = self.file.lock().expect("audit log mutex poisoned");
        g.write_all(&line)?;
        g.sync_data()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
#[path = "tests/audit.rs"]
mod tests;
