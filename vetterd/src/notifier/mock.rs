//! Test-driven [`Notifier`] backed by a Unix control socket.
//!
//! The integration tests in `vetterd/tests/daemon_e2e_prompt.rs` need
//! to drive prompt-class decisions deterministically without having a
//! macOS GUI session to click through `UNUserNotificationCenter`
//! banners. The pattern:
//!
//! 1. The test binds a `UnixListener` at a tempdir path before
//!    spawning the daemon.
//! 2. The daemon (started with `VETTERD_NOTIFIER=mock` and
//!    `VETTERD_NOTIFIER_SOCKET=<path>`) constructs a [`MockNotifier`]
//!    pointing at that path.
//! 3. On every [`notify`](Notifier::notify) call the notifier spawns
//!    a thread that connects to the socket, writes the prompt as a
//!    single newline-delimited JSON line, reads back a decision JSON
//!    line, and resolves the matching id in the
//!    [`PendingQueue`].
//!
//! Wire (newline-delimited JSON):
//!
//! ```jsonc
//! // daemon → test
//! {"id":"01J...","command":"curl","primary_verb":"GET",
//!  "primary_target":"https://example.test/","force_prompt":false,
//!  "rendered":" curl GET https://example.test/\n …"}
//!
//! // test → daemon
//! {"decision":"allow","reason":"test approved"}
//! ```
//!
//! `rendered` carries the same §8.5 detail string that
//! [`crate::pending::PendingQueue::pending_entries`] would surface to
//! the macOS popover; tests that want to assert on the popover-bound
//! payload can read it directly without having to wire AppKit.
//!
//! On any IO/parse error the mock falls back to a deny so a
//! misbehaving test driver can't accidentally turn into an allow.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vetter_core::wire::WireDecision;

use crate::pending::{PendingDecision, PendingQueue, PromptSummary};

use super::Notifier;

#[derive(Debug, Serialize, Deserialize)]
struct WireResponse {
    decision: String,
    #[serde(default)]
    reason: String,
}

pub struct MockNotifier {
    socket: PathBuf,
    queue: Arc<PendingQueue>,
}

impl MockNotifier {
    pub fn new(socket: PathBuf, queue: Arc<PendingQueue>) -> Self {
        Self { socket, queue }
    }

    fn ask(
        socket: &PathBuf,
        summary: &PromptSummary,
        rendered: &str,
    ) -> std::io::Result<PendingDecision> {
        let stream = UnixStream::connect(socket)?;
        // Independent read/write timeouts: a slow test driver should
        // produce a deny rather than wedge the daemon. 5s is plenty
        // for `accept → write → respond` in the same process.
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

        let mut writer = stream.try_clone()?;
        // Embed the rendered §8.5 detail alongside the summary so
        // the test driver can assert on what the popover would show.
        // Done as a flat object rather than a nested one so existing
        // mock-driver code that only deserialises the summary fields
        // keeps working (`#[serde(default)]` on the new field).
        let mut payload = serde_json::to_value(summary).map_err(io_err)?;
        if let Some(map) = payload.as_object_mut() {
            map.insert(
                "rendered".into(),
                serde_json::Value::String(rendered.into()),
            );
        }
        let mut req = serde_json::to_vec(&payload).map_err(io_err)?;
        req.push(b'\n');
        writer.write_all(&req)?;
        writer.flush()?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "control socket closed before sending decision",
            ));
        }
        let resp: WireResponse = serde_json::from_str(line.trim_end()).map_err(io_err)?;
        let decision = match resp.decision.as_str() {
            "allow" => WireDecision::Allow,
            "deny" => WireDecision::Deny,
            other => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown decision `{other}`"),
                ));
            }
        };
        Ok(PendingDecision {
            decision,
            reason: if resp.reason.is_empty() {
                "mock notifier".to_string()
            } else {
                resp.reason
            },
        })
    }
}

fn io_err<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

impl Notifier for MockNotifier {
    fn notify(&self, summary: &PromptSummary) {
        let socket = self.socket.clone();
        let queue = Arc::clone(&self.queue);
        let summary = summary.clone();
        // Look up the rendered §8.5 detail for this id from the
        // queue so we can include it in the wire payload. Empty
        // string when not found (legacy callers go through plain
        // `submit`).
        let rendered = queue
            .pending_entries()
            .into_iter()
            .find(|(s, _)| s.id == summary.id)
            .map(|(_, r)| r)
            .unwrap_or_default();
        // One thread per prompt so multiple in-flight prompts can be
        // resolved out of order. Tests rely on this for the
        // concurrent-prompts coverage.
        std::thread::spawn(move || {
            let id = summary.id.clone();
            let decision = match MockNotifier::ask(&socket, &summary, &rendered) {
                Ok(d) => d,
                Err(e) => PendingDecision::deny(format!("mock notifier error: {e}")),
            };
            queue.resolve(&id, decision);
        });
    }
}
