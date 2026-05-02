//! Hand-rolled host + path glob matchers. No regex dependency.
//!
//! Path globbing is segment-based: `*` matches one segment (no `/`),
//! `**` matches any number of segments (including zero). Matching is
//! intentionally narrow — `plans/Overview.md` §5 calls these out as
//! the only supported patterns; nothing else is silently allowed.

/// Match a hostname against a single pattern.
///
/// Supported patterns:
/// - exact (case-insensitive): `api.github.com` matches `api.github.com`.
/// - leading-`*.` glob: `*.github.com` matches `api.github.com` and
///   `a.b.github.com`, but NOT `github.com` itself (deliberate — listing
///   the apex requires a separate pattern; tests pin this).
///
/// Anything else is treated as an exact pattern, so `github*.com` only
/// matches the literal string `github*.com`. This rejects pathological
/// patterns rather than expanding them silently.
pub fn matches_host(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        if suffix.is_empty() {
            return false;
        }
        if host.eq_ignore_ascii_case(suffix) {
            return false;
        }
        let needle = format!(".{}", suffix);
        host.len() > needle.len()
            && host[(host.len() - needle.len())..].eq_ignore_ascii_case(&needle)
    } else {
        pattern.eq_ignore_ascii_case(host)
    }
}

/// Match a path against a glob.
///
/// Conventions:
/// - Leading slash is normalised away on both sides before comparison
///   (so `/repos/**` and `repos/**` behave identically).
/// - `*` matches a single segment (no embedded `/`).
/// - `**` matches zero or more segments. `/repos/**` matches `/repos`,
///   `/repos/foo`, and `/repos/foo/bar`.
/// - Anything else must match exactly (case-sensitive — paths are not
///   case-folded; that's a server-side concern).
pub fn matches_path(pattern: &str, path: &str) -> bool {
    let pat_segments: Vec<&str> = split_segments(pattern);
    let path_segments: Vec<&str> = split_segments(path);
    matches_segments(&pat_segments, &path_segments)
}

fn split_segments(s: &str) -> Vec<&str> {
    let trimmed = s.strip_prefix('/').unwrap_or(s);
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split('/').collect()
    }
}

fn matches_segments(pat: &[&str], path: &[&str]) -> bool {
    let mut p = 0usize;
    let mut s = 0usize;
    let mut star_p: Option<usize> = None;
    let mut star_s: usize = 0;

    while s <= path.len() {
        match pat.get(p) {
            Some(&"**") => {
                star_p = Some(p);
                star_s = s;
                p += 1;
            }
            Some(&pat_seg) if s < path.len() && matches_one(pat_seg, path[s]) => {
                p += 1;
                s += 1;
            }
            _ => {
                if let Some(sp) = star_p {
                    if star_s < path.len() {
                        star_s += 1;
                        s = star_s;
                        p = sp + 1;
                    } else {
                        return p == pat.len() && s == path.len();
                    }
                } else {
                    return s == path.len() && p == pat.len();
                }
            }
        }
        if p == pat.len() && s == path.len() {
            return true;
        }
    }
    false
}

fn matches_one(pat: &str, seg: &str) -> bool {
    if pat == "*" {
        return true;
    }
    pat == seg
}

#[cfg(test)]
#[path = "../tests/matcher_glob.rs"]
mod tests;
