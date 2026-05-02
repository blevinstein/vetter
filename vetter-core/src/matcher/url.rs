//! URL canonicalisation used before glob matching.
//!
//! [`normalise`] resolves `.` and `..` segments in the URL path so a
//! rule like `path: "/admin"` cannot be bypassed by a request to
//! `/admin/../secret`. The `url` crate already lowercases scheme and
//! host and removes default ports — we only have to handle the path.

use url::Url;

/// Return a copy of `u` with the path canonicalised:
/// - leading `/` preserved (or added back when stripped),
/// - `.` segments dropped,
/// - `..` segments pop the previous segment; popping above root is a
///   no-op (the path stays at root). This collapses common bypass
///   tricks before glob matching sees them.
pub fn normalise(u: &Url) -> Url {
    let mut out = u.clone();
    let normalised = normalise_path(u.path());
    out.set_path(&normalised);
    out
}

/// Canonicalise just the path portion. Returns a leading-`/` form
/// (`""` collapses to `"/"`).
pub fn normalise_path(path: &str) -> String {
    let trailing_slash = path.len() > 1 && path.ends_with('/');
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let mut stack: Vec<&str> = Vec::new();
    for segment in trimmed.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        return "/".to_string();
    }
    let mut result = String::with_capacity(path.len());
    for seg in &stack {
        result.push('/');
        result.push_str(seg);
    }
    if trailing_slash {
        result.push('/');
    }
    result
}

#[cfg(test)]
#[path = "../tests/matcher_url.rs"]
mod tests;
