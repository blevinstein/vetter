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
mod tests {
    use super::*;

    #[test]
    fn known_secret_headers_are_recognised() {
        for h in [
            "Authorization",
            "AUTHORIZATION",
            "authorization",
            "Cookie",
            "X-Api-Key",
            "x-api-key",
            "Proxy-Authorization",
            "X-Vault-Token",
            "X-Github-Token",
        ] {
            assert!(is_secret_header(h), "{h} should be secret");
        }
    }

    #[test]
    fn benign_headers_are_not_redacted() {
        for h in [
            "Content-Type",
            "Accept",
            "User-Agent",
            "X-Request-Id",
            "X-Correlation-Id",
            "X-Token-Issued-At",
        ] {
            assert!(!is_secret_header(h), "{h} should NOT be secret");
        }
    }

    #[test]
    fn redact_value_keeps_last_four_chars() {
        assert_eq!(redact_value("Bearer abcdef1234"), "••••1234");
    }

    #[test]
    fn redact_value_for_short_string() {
        assert_eq!(redact_value("abc"), "••••");
        assert_eq!(redact_value(""), "••••");
    }

    #[test]
    fn redact_value_does_not_split_multibyte() {
        let v = "héllo世界";
        let out = redact_value(v);
        assert!(out.starts_with("••••"));
        assert!(out.ends_with("lo世界"), "got `{out}`");
    }
}
