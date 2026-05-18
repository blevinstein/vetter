//! Header-value redaction.
//!
//! The renderer redacts every header value unconditionally — there is
//! no allowlist of "harmless" headers. Any value can carry secrets
//! the redaction list wouldn't have recognised (custom auth headers,
//! tenant identifiers, signed `Referer` URLs, JWTs in unusual
//! places, etc.), and "guess wrong, leak the value" is a worse
//! failure mode than "always redact, occasionally hide a benign
//! value".
//!
//! This module therefore exposes only [`redact_value`], the recipe
//! that turns a raw header value into the `••••<last4>` shape used
//! in the §8.5 layout. The list of "auth-bearing" header names that
//! used to live here moved to [`crate::signals::is_auth_header`],
//! which is the single source of truth for the `auth-header` risk
//! signal *and* the curl parser's `Auth::Header { name }` emission.

/// `••••<last4>` of a value, or just `••••` if the value is shorter
/// than 4 chars. Operates on chars, not bytes, to avoid splitting a
/// multibyte sequence.
pub fn redact_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 4 {
        return "••••".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

#[cfg(test)]
#[path = "../tests/render_redact.rs"]
mod tests;
