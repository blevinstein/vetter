//! `parsed.effects` → per-row summary strings.
//!
//! Each effect on a card becomes a row: a headers list, a body
//! summary, an auth line, a file path, a spawned command. The
//! *wording* of those rows — and the rules about which ones appear
//! at all — is presentation policy shared by every approval
//! surface; only the glyphs and stack views are toolkit work.
//!
//! Everything here is derived from `vetter_core::parsers` types plus
//! the redaction helpers in `vetter_core::render`, never from
//! `command` or anything outside the parsed command. That keeps the
//! renderer → card symmetry called out in
//! `plans/ApprovalUI.md "Goals"`.
//!
//! Strings that embed parser output are sanitised here rather than
//! at each call site, so no surface can accidentally paint raw argv
//! bytes into a row a human is about to approve.

use std::collections::HashSet;
use std::path::PathBuf;

use vetter_core::render::sanitize_for_display;
use vetter_core::{Auth, Body, Effect, FormField, ParsedCommand};

/// Gather every `Body::FromFile` path across this parsed command's
/// `HttpRequest` effects.
///
/// The curl parser intentionally emits **both** `Body::FromFile {
/// path }` (for `-d @file`) **and** a separate `Effect::FileRead {
/// path }` so the matcher and audit log can reason about the file
/// read independently of the HTTP body. That double-bookkeeping is
/// invisible in the §8.5 renderer (one "body" line, one "files"
/// line) but a card would render two identical rows — body row plus
/// read row, same path — without help. Callers skip any `FileRead`
/// whose path appears in this set; the body row already exposes the
/// path and its Open button. The `FileRead` stays in the underlying
/// effect list (the matcher still sees it) — only the visible row
/// collapses.
pub fn collect_body_file_paths(parsed: &ParsedCommand) -> HashSet<PathBuf> {
    let mut out: HashSet<PathBuf> = HashSet::new();
    for eff in &parsed.effects {
        if let Effect::HttpRequest(req) = eff {
            if let Body::FromFile { path } = &req.body {
                out.insert(path.clone());
            }
        }
    }
    out
}

/// One-line summary of a request body, e.g. `inline, 7 B`.
///
/// `None` for `Body::None` so a bodyless GET doesn't get a stub
/// "body" section.
pub fn body_meta_label(body: &Body) -> Option<String> {
    match body {
        Body::None => None,
        Body::Inline { bytes } => Some(format!("inline, {} B", bytes.len())),
        Body::FromFile { .. } => Some("from file".to_string()),
        Body::Form { fields } => Some(format!("x-www-form-urlencoded, {} fields", fields.len())),
    }
}

/// Render inline body bytes either as a UTF-8 string (when valid)
/// or a short hex dump (when not), truncated to the first 64 bytes
/// with a trailing count. Bodies in v1 are usually short (form
/// posts, JSON deltas); long bodies stay reachable via "Show raw".
pub fn inline_body_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => sanitize_for_display(s).into_owned(),
        Err(_) => {
            let max = 64.min(bytes.len());
            let hex: String = bytes[..max]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            if max < bytes.len() {
                format!("{hex} … ({} more bytes)", bytes.len() - max)
            } else {
                hex
            }
        }
    }
}

/// `name=value` text for one urlencoded form field.
pub fn form_field_text(field: &FormField) -> String {
    format!(
        "{}={}",
        sanitize_for_display(&field.name),
        sanitize_for_display(&field.value)
    )
}

/// Auth row text, plus whether the credential was redacted out of
/// the UI.
///
/// The redaction flag drives colour: **green** when the credential
/// never reaches the screen (the safe state — the agent has auth and
/// we kept the secret off-screen), muted otherwise (today only
/// `Auth::Netrc`, where the credential never reaches us at all).
/// Deliberately *not* an alarm colour for redacted credentials:
/// having auth on a request is generally a good sign, and a red row
/// read as "secret leaked" when the opposite is true.
pub fn auth_label(auth: &Auth) -> (String, bool) {
    match auth {
        Auth::Basic {
            user,
            password_redacted,
        } => (
            format!("Basic user={} password=••••", sanitize_for_display(user)),
            *password_redacted,
        ),
        Auth::Bearer { token_redacted } => ("Bearer ••••".to_string(), *token_redacted),
        Auth::Header { name } => (format!("{}: ••••", sanitize_for_display(name)), true),
        Auth::Netrc => ("from .netrc".to_string(), false),
    }
}

#[cfg(test)]
#[path = "../tests/cards_effects.rs"]
mod tests;
