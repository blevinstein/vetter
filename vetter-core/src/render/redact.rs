//! Header-name and value redaction. Hard-coded list per
//! `plans/Overview.md` §8.5.

const SECRET_HEADER_NAMES_LC: &[&str] = &[
    "authorization",
    "cookie",
    "x-api-key",
    "proxy-authorization",
];

/// True if the header name should have its value redacted before
/// reaching the rendered summary.
pub fn is_secret_header(name: &str) -> bool {
    let lc = name.to_ascii_lowercase();
    if SECRET_HEADER_NAMES_LC.contains(&lc.as_str()) {
        return true;
    }
    // x-<something>-token glob from §8.5 / §9.
    if let Some(rest) = lc.strip_prefix("x-") {
        if let Some(prefix) = rest.strip_suffix("-token") {
            if !prefix.is_empty() {
                return true;
            }
        }
    }
    false
}

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
