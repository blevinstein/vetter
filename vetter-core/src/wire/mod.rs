//! JSON wire types for the Unix-socket protocol between `vet` and `vetterd`.
//!
//! Implements the schema sketched in `plans/Overview.md` §3. The
//! protocol is intentionally synchronous (one request → one decision);
//! concurrency is achieved by spawning a worker thread per connection
//! on the daemon side. Frames are length-prefixed JSON: a 4-byte
//! big-endian `u32` length followed by exactly that many bytes of
//! UTF-8 JSON.
//!
//! Both message types include a `v` field. A peer that sees a `v` it
//! does not understand returns [`WireError::VersionMismatch`] without
//! attempting to parse the rest — this keeps the daemon defensive
//! against rolled-back clients.
//!
//! ## Versioning
//!
//! - **v1** (Phase 3a): the client sent a pre-parsed [`ParsedCommand`]
//!   in `VetRequest.parsed` and the daemon evaluated against that
//!   field directly. The daemon trusted argv-shaped lies — the
//!   attack documented as T2 in `plans/ThreatModel.md`.
//! - **v2** (Phase 3b, current): `VetRequest` carries only the raw
//!   argv (plus `cwd`, agent hint, and a correlation id). The daemon
//!   re-runs the parser on argv and rules match on the *daemon's*
//!   `ParsedCommand`. A same-UID attacker can no longer submit
//!   `argv: ["curl", "https://evil.com"]` with parsed effects
//!   describing a benign call.

use std::io::{Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::matcher::Decision as MatchDecision;

/// The wire-format version this build speaks. Bumping this requires
/// either a backward-compatible schema (default-able new fields) or
/// negotiating `v` on the connection.
pub const PROTOCOL_VERSION: u32 = 2;

/// Hard cap on a single frame body. The daemon refuses anything larger
/// rather than allocating an unbounded `Vec`. 1 MiB is generous: a
/// realistic v2 [`VetRequest`] is just argv + a few small strings,
/// comfortably under 64 KiB even for pathological argvs. The cap
/// exists to defend against accidentally-corrupt length prefixes on
/// real connections and against deliberately-malformed input from
/// fuzzing in Phase 3b.
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// Sent by `vet` after rendering a local preview of the wrapped
/// command. The daemon re-parses [`Self::argv`], evaluates the result
/// against its allowlist store, and returns a [`VetDecision`].
///
/// Wire schema is intentionally minimal: anything the daemon could
/// conclude on its own (parsed effects, command name, stdin digest)
/// is *not* on the wire — see the v1→v2 transition note in the
/// module docstring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VetRequest {
    /// Wire-protocol version. Must equal [`PROTOCOL_VERSION`] for the
    /// peer to accept the message.
    pub v: u32,
    /// Correlation id (ULID, time-sortable). Echoed back in the
    /// matching [`VetDecision`] and in the audit log entry.
    pub id: String,
    /// Working directory `vet` was invoked from. Daemon uses this for
    /// project-scope rule discovery and as the `cwd` field on the
    /// re-parsed [`crate::ParsedCommand`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<std::path::PathBuf>,
    /// Best-effort agent identifier (`"claude-code"`, `"cursor"`,
    /// etc.) sniffed from env variables on the client side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_hint: Option<String>,
    /// Original argv as the user invoked it. `argv[0]` is the parser
    /// dispatch key on the daemon side; the rest is fed verbatim into
    /// the parser. **The daemon never trusts argv-derived effects
    /// the client may have computed locally** — that's the whole
    /// point of the v2 wire break.
    pub argv: Vec<String>,
    /// Force the request onto the prompt path, even if a permissive
    /// rule would otherwise auto-allow. `vet --dry-run` rides this
    /// flag.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force_prompt: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Daemon's reply to a [`VetRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VetDecision {
    pub v: u32,
    /// Echoes [`VetRequest::id`] so the client can sanity-check.
    pub id: String,
    pub decision: WireDecision,
    /// Human-readable explanation used by the renderer's match line
    /// and by `vet`'s `allow (…)` / `deny (…)` line on stderr.
    pub reason: String,
    /// If the approver chose "Allowlist…" this carries the new rule
    /// the daemon persisted (Phase 4+). Always `None` in 3b since
    /// the stub UI never adds rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_added: Option<serde_json::Value>,
}

/// Top-level decision the daemon returns. Mirrors `plans/Overview.md`
/// §3 and is intentionally a separate type from
/// [`crate::matcher::Decision`]: the matcher type carries `rule_id`
/// and `scope`, but the wire only needs a flat tag plus a free-form
/// `reason` so it can describe outcomes the matcher doesn't know
/// about (`"no UI yet"`, `"version mismatch"`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDecision {
    Allow,
    Deny,
    /// Reserved for the Phase-4 UI: a one-shot allow not persisted to
    /// any allowlist file. In Phase 3 daemons never emit this; if a
    /// peer-of-the-future sends it the client should treat it
    /// conservatively (Phase 3 `vet` maps it to deny).
    AllowOnce,
}

impl WireDecision {
    /// Project a [`crate::matcher::Decision`] onto the wire. The
    /// daemon stub UI is responsible for the `Prompt → Deny "no UI
    /// yet"` mapping (kept out of this helper so the caller controls
    /// the human-readable reason).
    pub fn from_match(d: &MatchDecision) -> Option<Self> {
        match d {
            MatchDecision::Allow { .. } => Some(WireDecision::Allow),
            MatchDecision::Deny { .. } => Some(WireDecision::Deny),
            MatchDecision::Prompt => None,
        }
    }
}

/// Errors raised by the framing/serde layer. All are recoverable on
/// the daemon side (close the connection and accept the next one);
/// the client converts them into an exit-78 with a human-readable
/// message.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame body of {len} bytes exceeds the {max} cap")]
    FrameTooLarge { len: u32, max: u32 },
    #[error("connection closed mid-frame after {read} of {expected} bytes")]
    Truncated { read: u64, expected: u32 },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("peer wire version is {got}; this build speaks {want}")]
    VersionMismatch { got: u32, want: u32 },
    #[error("decision id `{got}` does not match expected `{expected}`")]
    IdMismatch { got: String, expected: String },
    /// Peer-credential check on the connected stream failed: the
    /// process on the other end of the socket runs as a different UID
    /// from us. Either the daemon path was hijacked by another user,
    /// or `$VETTERD_SOCKET` points at a foreign service. Either way
    /// `vet` aborts without sending the request body.
    #[error(
        "peer authentication failed: socket peer uid {peer} does not match expected uid {expected}"
    )]
    PeerAuth { expected: u32, peer: u32 },
}

/// Serialise `msg` to JSON, write a 4-byte big-endian length prefix,
/// then the body. Flushes the writer so the peer sees the frame
/// without having to wait for buffered IO.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), WireError> {
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len()).map_err(|_| WireError::FrameTooLarge {
        len: u32::MAX,
        max: MAX_FRAME_BYTES,
    })?;
    if len > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

/// Read a length-prefixed frame and decode the JSON body into `T`.
///
/// Rejects frames larger than [`MAX_FRAME_BYTES`] **before** allocating
/// the body buffer, so a hostile peer cannot exhaust memory by
/// announcing a 4 GiB frame.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<T, WireError> {
    let mut len_buf = [0u8; 4];
    read_exact_or_truncated(r, &mut len_buf, 0)?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        return Err(WireError::FrameTooLarge {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    let mut body = vec![0u8; len as usize];
    read_exact_or_truncated(r, &mut body, len)?;
    let msg: T = serde_json::from_slice(&body)?;
    Ok(msg)
}

fn read_exact_or_truncated<R: Read>(
    r: &mut R,
    buf: &mut [u8],
    expected: u32,
) -> Result<(), WireError> {
    let mut read = 0usize;
    while read < buf.len() {
        match r.read(&mut buf[read..]) {
            Ok(0) => {
                return Err(WireError::Truncated {
                    read: read as u64,
                    expected,
                });
            }
            Ok(n) => read += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(WireError::Io(e)),
        }
    }
    Ok(())
}

/// Convenience wrapper around [`read_frame`] that also enforces
/// [`PROTOCOL_VERSION`]. Daemons read requests with this; clients
/// read decisions with this. Either side is free to call the lower-
/// level [`read_frame`] when it wants to inspect `v` itself first.
pub fn read_request<R: Read>(r: &mut R) -> Result<VetRequest, WireError> {
    let req: VetRequest = read_frame(r)?;
    if req.v != PROTOCOL_VERSION {
        return Err(WireError::VersionMismatch {
            got: req.v,
            want: PROTOCOL_VERSION,
        });
    }
    Ok(req)
}

/// Counterpart to [`read_request`] for the client side.
pub fn read_decision<R: Read>(r: &mut R, expected_id: &str) -> Result<VetDecision, WireError> {
    let dec: VetDecision = read_frame(r)?;
    if dec.v != PROTOCOL_VERSION {
        return Err(WireError::VersionMismatch {
            got: dec.v,
            want: PROTOCOL_VERSION,
        });
    }
    if dec.id != expected_id {
        return Err(WireError::IdMismatch {
            got: dec.id,
            expected: expected_id.to_string(),
        });
    }
    Ok(dec)
}

// ── Management channel ──────────────────────────────────────────────────────

/// Compact summary of one pending prompt, serialised over the admin socket.
/// Mirrors `vetterd::pending::PromptSummary` but lives in `vetter-core` so
/// the `vet` CLI can decode it without depending on `vetterd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingItem {
    /// Correlation id, equal to [`VetRequest::id`].
    pub id: String,
    /// Stable parser identifier, e.g. `"curl"`.
    pub command: String,
    /// Display verb (e.g. `"GET"`, `"POST"`). Empty when the parser didn't
    /// set one.
    pub primary_verb: String,
    /// Display target (typically a normalised URL).
    pub primary_target: String,
    /// True when the request arrived via `vet --dry-run`.
    pub force_prompt: bool,
}

/// Request sent by `vet` to the admin socket. Uses a serde-tagged enum so
/// additional management operations can be added in future without a version
/// bump on the main socket protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MgmtRequest {
    /// Return all requests currently parked in the pending-prompt queue.
    ListPending,
}

/// Response returned by `vetterd` on the admin socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MgmtResponse {
    PendingList { items: Vec<PendingItem> },
    Error { message: String },
}

/// Generate a fresh ULID-based correlation id. Time-sortable and
/// monotonic per process; ideal for the audit log so entries appear
/// in chronological order without needing a separate timestamp index.
pub fn new_request_id() -> String {
    ulid::Ulid::new().to_string()
}

#[cfg(test)]
#[path = "../tests/wire.rs"]
mod tests;
