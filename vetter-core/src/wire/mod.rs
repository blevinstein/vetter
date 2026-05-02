//! JSON wire types for the Unix-socket protocol between `vet` and `vetterd`.
//!
//! Implements the schema sketched in `plans/Overview.md` §3. Phase 3a
//! ships v1 with one [`VetRequest`] frame per connection followed by
//! one [`VetDecision`] frame back. Frames are length-prefixed JSON: a
//! 4-byte big-endian `u32` length followed by exactly that many bytes
//! of UTF-8 JSON. The protocol is intentionally synchronous (one
//! request → one decision); concurrency is achieved by spawning a
//! worker thread per connection on the daemon side.
//!
//! Both message types include a `v` field. A peer that sees a `v` it
//! does not understand returns [`WireError::VersionMismatch`] without
//! attempting to parse the rest — this keeps the daemon defensive
//! against rolled-back clients.

use std::io::{Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::matcher::Decision as MatchDecision;
use crate::ParsedCommand;

/// The wire-format version this build speaks. Bumping this requires
/// either a backward-compatible schema (default-able new fields) or
/// negotiating `v` on the connection.
pub const PROTOCOL_VERSION: u32 = 1;

/// Hard cap on a single frame body. The daemon refuses anything larger
/// rather than allocating an unbounded `Vec`. 4 MiB is generous: a
/// realistic [`VetRequest`] (with `parsed.effects` + the original
/// argv) is well under 64 KiB even with a 1 MiB inline body. The cap
/// exists to defend against accidentally-corrupt length prefixes on
/// real connections and against deliberately-malformed input from
/// fuzzing in Phase 3b.
pub const MAX_FRAME_BYTES: u32 = 4 * 1024 * 1024;

/// Sent by `vet` after it has parsed the wrapped command and rendered
/// the summary locally. The daemon evaluates this against its
/// allowlist store and returns a [`VetDecision`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VetRequest {
    /// Wire-protocol version. Must equal [`PROTOCOL_VERSION`] for the
    /// peer to accept the message.
    pub v: u32,
    /// Correlation id (ULID, time-sortable). Echoed back in the
    /// matching [`VetDecision`] and in the audit log entry.
    pub id: String,
    /// Working directory `vet` was invoked from. Daemon uses this for
    /// project-scope rule discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<std::path::PathBuf>,
    /// Best-effort agent identifier (`"claude-code"`, `"cursor"`,
    /// etc.) sniffed from env variables on the client side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_hint: Option<String>,
    /// The wrapped command name (parser identifier, `"curl"` etc.).
    pub command: String,
    /// Original argv as the user invoked it; preserved for the audit
    /// log and for any future re-parsing on the daemon side.
    pub argv: Vec<String>,
    /// SHA-256 of any stdin bytes the client consumed for the parser
    /// (e.g. curl's `-d @-`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_digest: Option<String>,
    /// Parsed structured surface — the canonical thing rules match on.
    pub parsed: ParsedCommand,
    /// Force the request onto the prompt path, even if a permissive
    /// rule would otherwise auto-allow. `vet --dry-run` rides this
    /// flag in Phase 3a (when the prompt path stub-denies, dry-run
    /// effectively becomes "always refuse").
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
    /// the daemon persisted (Phase 4+). Always `None` in 3a since
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
    /// any allowlist file. In Phase 3a daemons never emit this; if a
    /// peer-of-the-future sends it the client should treat it
    /// conservatively (Phase 3a `vet` maps it to deny).
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

/// Generate a fresh ULID-based correlation id. Time-sortable and
/// monotonic per process; ideal for the audit log so entries appear
/// in chronological order without needing a separate timestamp index.
pub fn new_request_id() -> String {
    ulid::Ulid::new().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DisplayHints, Effect, HttpMethod, HttpRequest, ParsedCommand, TlsPolicy};
    use std::io::Cursor;

    fn sample_parsed() -> ParsedCommand {
        ParsedCommand {
            command: "curl".into(),
            argv: vec!["curl".into(), "https://example.test/".into()],
            cwd: None,
            stdin_digest: None,
            effects: vec![Effect::HttpRequest(HttpRequest {
                method: HttpMethod::Get,
                url: url::Url::parse("https://example.test/").unwrap(),
                headers: vec![],
                body: crate::Body::None,
                auth: None,
                tls: TlsPolicy::Strict,
                follow_redirects: false,
                proxy: None,
            })],
            signals: vec![],
            display_hints: DisplayHints::default(),
            extras: serde_json::Value::Null,
        }
    }

    fn sample_request(id: &str) -> VetRequest {
        VetRequest {
            v: PROTOCOL_VERSION,
            id: id.to_string(),
            cwd: Some("/work".into()),
            agent_hint: Some("claude-code".into()),
            command: "curl".into(),
            argv: vec!["curl".into(), "https://example.test/".into()],
            stdin_digest: None,
            parsed: sample_parsed(),
            force_prompt: false,
        }
    }

    fn sample_decision(id: &str) -> VetDecision {
        VetDecision {
            v: PROTOCOL_VERSION,
            id: id.to_string(),
            decision: WireDecision::Allow,
            reason: "matched rule `r` in user".into(),
            rule_added: None,
        }
    }

    #[test]
    fn request_roundtrips_through_frame() {
        let req = sample_request("01HX0000000000000000000000");
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let mut cur = Cursor::new(buf);
        let back: VetRequest = read_frame(&mut cur).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn decision_roundtrips_through_frame() {
        let dec = sample_decision("01HX0000000000000000000000");
        let mut buf = Vec::new();
        write_frame(&mut buf, &dec).unwrap();
        let mut cur = Cursor::new(buf);
        let back: VetDecision = read_frame(&mut cur).unwrap();
        assert_eq!(dec, back);
    }

    #[test]
    fn new_request_id_returns_a_ulid_string() {
        let id = new_request_id();
        assert_eq!(id.len(), 26, "ULID should be 26 chars: {id}");
        let parsed = ulid::Ulid::from_string(&id).unwrap();
        assert!(parsed.timestamp_ms() > 0);
    }

    #[test]
    fn version_mismatch_rejected_by_helpers() {
        let mut req = sample_request("01HX0000000000000000000000");
        req.v = 99;
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let mut cur = Cursor::new(buf);
        let err = read_request(&mut cur).unwrap_err();
        assert!(matches!(
            err,
            WireError::VersionMismatch { got: 99, want: 1 }
        ));
    }

    #[test]
    fn read_decision_validates_id_match() {
        let dec = sample_decision("AAA");
        let mut buf = Vec::new();
        write_frame(&mut buf, &dec).unwrap();
        let mut cur = Cursor::new(buf);
        let err = read_decision(&mut cur, "BBB").unwrap_err();
        assert!(matches!(err, WireError::IdMismatch { .. }), "{err}");
    }

    #[test]
    fn oversized_frame_rejected_before_alloc() {
        // Hand-build a length prefix announcing 5 MiB; the body should
        // never be read because the cap is checked first.
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_be_bytes());
        let mut cur = Cursor::new(buf);
        let err: WireError = read_frame::<_, VetRequest>(&mut cur).unwrap_err();
        assert!(matches!(err, WireError::FrameTooLarge { .. }), "{err}");
    }

    #[test]
    fn truncated_body_reported() {
        let req = sample_request("01HX0000000000000000000000");
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        buf.truncate(buf.len() - 5);
        let mut cur = Cursor::new(buf);
        let err = read_frame::<_, VetRequest>(&mut cur).unwrap_err();
        assert!(matches!(err, WireError::Truncated { .. }), "{err}");
    }

    #[test]
    fn truncated_length_prefix_reported() {
        let mut cur = Cursor::new(vec![0u8, 0u8]);
        let err = read_frame::<_, VetRequest>(&mut cur).unwrap_err();
        assert!(matches!(err, WireError::Truncated { .. }), "{err}");
    }

    #[test]
    fn force_prompt_round_trips_when_set() {
        let mut req = sample_request("01HX0000000000000000000000");
        req.force_prompt = true;
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        let mut cur = Cursor::new(buf);
        let back: VetRequest = read_frame(&mut cur).unwrap();
        assert!(back.force_prompt);
    }

    #[test]
    fn force_prompt_omitted_from_json_when_false() {
        let req = sample_request("01HX0000000000000000000000");
        let s = serde_json::to_string(&req).unwrap();
        assert!(
            !s.contains("force_prompt"),
            "default false should not appear: {s}"
        );
    }

    #[test]
    fn from_match_maps_decisions_correctly() {
        use crate::matcher::Scope;
        assert_eq!(
            WireDecision::from_match(&MatchDecision::Allow {
                rule_id: "r".into(),
                scope: Scope::User
            }),
            Some(WireDecision::Allow)
        );
        assert_eq!(
            WireDecision::from_match(&MatchDecision::Deny {
                rule_id: "r".into(),
                scope: Scope::Denylist
            }),
            Some(WireDecision::Deny)
        );
        assert_eq!(WireDecision::from_match(&MatchDecision::Prompt), None);
    }
}
