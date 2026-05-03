//! JSON-lines audit log.
//!
//! Per `plans/Overview.md` §7 and `plans/TestingPlan.md` §5.4, every
//! decision (auto-allow, auto-deny, eventual human-allow, ...) is
//! appended as one JSON line. The file is `sync_data`-flushed before
//! the daemon writes the decision frame back to the client, so a
//! crash between "decision sent" and "next request" never loses an
//! audit entry.
//!
//! Prompt-class entries additionally carry the `PromptSummary`-
//! derived fields (`primary_verb`, `primary_target`, `signals`,
//! `parsed`, `host_known`) plus the pre-rendered §8.5 detail string.
//! On daemon startup [`AuditLog::tail_prompt_entries`] tails those
//! entries from the file's end to rehydrate the
//! [`crate::pending::PendingQueue`]'s resolved-history ring so the
//! popover "Recent" section survives a restart.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use vetter_core::wire::WireDecision;
use vetter_core::{ParsedCommand, RiskSignal};

/// One audit-log line. JSON-Lines (no trailing comma, terminated with
/// `\n`). Fields chosen per `plans/TestingPlan.md` §5.4 — `id`,
/// `timestamp`, `command`, `argv`, `decision`, `reason`, `rule_id`.
/// Phase 3a adds `force_prompt` so a `--dry-run`-driven entry is
/// distinguishable from a real prompt-class miss.
///
/// The trailing optional block (`primary_verb` … `rendered`) carries
/// everything the popover's resolved-history ring needs to repaint a
/// card. These fields are populated only for prompt-class rows —
/// auto-allow / auto-deny rows leave them empty (and `skip_serializing_if`
/// keeps those rows compact on disk). A non-empty `rendered` is the
/// marker [`AuditLog::tail_prompt_entries`] uses to distinguish a
/// prompt row from an auto-decision row.
///
/// `PartialEq` (not `Eq`): `ParsedCommand` carries `serde_json::Value`
/// in `extras`, which has no `Eq` impl. Same reason `PromptSummary`
/// lost its `Eq` derive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Mirrors [`crate::pending::PromptSummary::primary_verb`].
    /// Empty on auto-decision rows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub primary_verb: String,
    /// Mirrors [`crate::pending::PromptSummary::primary_target`].
    /// Empty on auto-decision rows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub primary_target: String,
    /// Mirrors [`crate::pending::PromptSummary::signals`]. Empty on
    /// auto-decision rows (and on prompt rows that produced no
    /// signals — which is fine, the popover just skips the pills row).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<RiskSignal>,
    /// Mirrors [`crate::pending::PromptSummary::parsed`]. `None` on
    /// auto-decision rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed: Option<ParsedCommand>,
    /// Mirrors [`crate::pending::PromptSummary::host_known`]. Empty
    /// on auto-decision rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_known: Vec<bool>,
    /// Pre-rendered §8.5 detail string (ANSI-escaped) — same string
    /// the popover displayed while the request was pending. Empty
    /// on auto-decision rows; presence of a non-empty value is what
    /// `tail_prompt_entries` keys on to tell prompt rows apart from
    /// auto rows.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rendered: String,
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

    /// Tail up to `cap` prompt-class entries from the audit log,
    /// newest first. Used at daemon startup to rehydrate the
    /// popover's resolved-history ring without reading the whole
    /// file.
    ///
    /// Implementation: read a window of `TAIL_WINDOW_START` bytes
    /// from the end of the file, discard the first (possibly
    /// partial) line if we didn't start at byte 0, then split on
    /// `\n` and parse each line with `serde_json::from_slice`.
    /// Prompt rows are distinguished by a non-empty `rendered`
    /// field; auto-decision rows and lines that fail to parse are
    /// silently skipped. If we collected fewer than `cap` prompt
    /// rows and the window wasn't the whole file yet, we double
    /// and retry.
    ///
    /// Returns up to `cap` entries in newest-first order (the
    /// order [`crate::pending::PendingQueue::warm_resolved`]
    /// consumes). IO errors bubble up; the daemon treats them as
    /// non-fatal and starts with an empty ring.
    pub fn tail_prompt_entries(&self, cap: usize) -> std::io::Result<Vec<AuditEntry>> {
        if cap == 0 {
            return Ok(Vec::new());
        }
        let mut file = OpenOptions::new().read(true).open(&self.path)?;
        let end = file.seek(SeekFrom::End(0))?;
        if end == 0 {
            return Ok(Vec::new());
        }

        let mut window: u64 = TAIL_WINDOW_START;
        loop {
            let effective = std::cmp::min(window, end);
            let pos = end - effective;
            file.seek(SeekFrom::Start(pos))?;
            let mut buf = vec![0u8; effective as usize];
            file.read_exact(&mut buf)?;

            // Lines, skipping the first split if pos > 0 (it may be
            // a partial line whose head is earlier in the file).
            let mut splits = buf.split(|&b| b == b'\n');
            if pos > 0 {
                splits.next();
            }

            // Walk oldest → newest in the window, parse prompt rows.
            let prompt_rows: Vec<AuditEntry> = splits
                .filter(|l| !l.is_empty() && looks_like_prompt_row(l))
                .filter_map(|l| serde_json::from_slice::<AuditEntry>(l).ok())
                .filter(|e| !e.rendered.is_empty())
                .collect();

            if prompt_rows.len() >= cap || pos == 0 {
                let start = prompt_rows.len().saturating_sub(cap);
                // Newest-first for the caller.
                let out: Vec<AuditEntry> = prompt_rows[start..].iter().rev().cloned().collect();
                return Ok(out);
            }

            // Not enough prompt rows yet; expand the window and
            // retry. `end` is the hard upper bound so we terminate
            // on the next iteration if we haven't already.
            window = window.saturating_mul(2);
            if window >= end {
                window = end;
            }
        }
    }
}

/// Cheap pre-filter: prompt rows always serialise with a non-empty
/// `"rendered":"…"` field (the §8.5 detail string), while auto-
/// decision rows skip it. Testing for the literal needle short-
/// circuits the expensive serde parse on the common case.
fn looks_like_prompt_row(line: &[u8]) -> bool {
    const NEEDLE: &[u8] = br#""rendered":""#;
    if line.len() < NEEDLE.len() {
        return false;
    }
    line.windows(NEEDLE.len()).any(|w| w == NEEDLE)
}

/// Initial read-window size for [`AuditLog::tail_prompt_entries`].
/// 64 KiB is enough to cover ~20 prompt rows in the common case
/// (one parsed curl with a few headers serialises to ~1–3 KiB);
/// the caller doubles the window on miss until either `cap` rows
/// are collected or the whole file has been read.
const TAIL_WINDOW_START: u64 = 64 * 1024;

#[cfg(test)]
#[path = "tests/audit.rs"]
mod tests;
