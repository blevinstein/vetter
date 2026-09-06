//! Tests for [`crate::cards::url`].
//!
//! The host-trust and loopback assertions were lifted out of
//! `crate::tests::popover_url` when the classifier moved into the
//! shared card layer; they never needed AppKit and now run on every
//! target. The URL-segmentation assertions are new — that logic was
//! previously inline in `build_url_row` and reachable only through a
//! main-thread AppKit call, so it had no direct coverage at all.

use super::*;

fn url(s: &str) -> url::Url {
    url::Url::parse(s).expect("valid test URL")
}

#[test]
fn host_trust_loopback_wins_over_known_flag() {
    // The loopback rule must fire even if the daemon's
    // `host_known` hint says "false" — a stale store should never
    // paint `localhost` as scary orange.
    assert_eq!(host_trust("localhost", false), HostTrust::Loopback);
    assert_eq!(host_trust("LOCALHOST", false), HostTrust::Loopback);
    assert_eq!(host_trust("127.0.0.1", false), HostTrust::Loopback);
    assert_eq!(host_trust("[::1]", false), HostTrust::Loopback);
    // Even if the store does say known, loopback still classifies
    // as loopback (so the loopback pill renders, not the green one).
    assert_eq!(host_trust("localhost", true), HostTrust::Loopback);
}

#[test]
fn host_trust_routes_known_and_unknown() {
    assert_eq!(host_trust("api.example.com", true), HostTrust::Known);
    assert_eq!(host_trust("never.heard.test", false), HostTrust::Unknown);
}

#[test]
fn is_loopback_matches_ipv4_and_ipv6_literals() {
    assert!(is_loopback("localhost"));
    assert!(is_loopback("127.0.0.1"));
    assert!(is_loopback("127.255.255.254"));
    assert!(is_loopback("[::1]"));
    assert!(is_loopback("::1"));
    assert!(!is_loopback("8.8.8.8"));
    assert!(!is_loopback("example.com"));
}

#[test]
fn each_trust_class_has_its_own_tooltip() {
    // The tooltip is what explains the pill's colour on hover, so a
    // duplicate would leave two classes indistinguishable.
    let all = [HostTrust::Loopback, HostTrust::Known, HostTrust::Unknown];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a.tooltip(), b.tooltip(), "{a:?} vs {b:?}");
        }
    }
    assert!(HostTrust::Loopback.tooltip().contains("loopback"));
}

#[test]
fn quiet_ports_are_hidden_and_others_are_shown() {
    // Explicit `:443` on an https URL is normalised away by the url
    // crate, so the interesting cases are the dev-stack ports we
    // choose to stay quiet about versus a genuinely odd one.
    assert_eq!(visible_port(&url("https://x.test:8080/")), None);
    assert_eq!(visible_port(&url("https://x.test:8443/")), None);
    assert_eq!(visible_port(&url("http://x.test:80/")), None);
    assert_eq!(visible_port(&url("https://x.test/")), None);
    assert_eq!(visible_port(&url("https://x.test:9000/")), Some(9000));
}

#[test]
fn path_query_folds_path_and_query_into_one_tail() {
    let pq = path_query(&url("https://x.test/v1/things?q=1"));
    assert_eq!(pq.text, "/v1/things?q=1");
    assert!(pq.has_path);
}

#[test]
fn path_query_keeps_a_bare_slash_only_when_it_is_the_whole_target() {
    // `https://host/` → a dim `/`, because that *is* the target.
    let root = path_query(&url("https://x.test/"));
    assert_eq!(root.text, "/");
    assert!(!root.has_path);

    // With a query the leading `/` would be redundant next to `?`.
    let query_only = path_query(&url("https://x.test/?q=1"));
    assert_eq!(query_only.text, "/?q=1");
    assert!(!query_only.has_path);
}

#[test]
fn path_query_marks_a_real_path_so_it_renders_undimmed() {
    // `has_path` is the dim/undim switch: a concrete path is a
    // location, a bare query is a search.
    assert!(path_query(&url("https://x.test/a/b")).has_path);
    assert!(!path_query(&url("https://x.test/?a=b")).has_path);
}

#[test]
fn path_query_sanitises_control_bytes() {
    // A path carrying an RTLO override must not reach a card
    // verbatim — it could reorder what the human reads before
    // approving. `url` percent-encodes most of this, so assert the
    // invariant rather than an exact placeholder.
    let pq = path_query(&url("https://x.test/a\u{202e}b"));
    assert!(
        !pq.text.contains('\u{202e}'),
        "RTLO survived into the URL tail: {:?}",
        pq.text
    );
}

#[test]
fn method_tone_follows_the_renderer_taxonomy() {
    use vetter_core::HttpMethod;
    assert_eq!(method_tone(&HttpMethod::Get), MethodTone::Read);
    assert_eq!(method_tone(&HttpMethod::Head), MethodTone::Read);
    assert_eq!(method_tone(&HttpMethod::Options), MethodTone::Read);
    assert_eq!(method_tone(&HttpMethod::Trace), MethodTone::Read);
    assert_eq!(method_tone(&HttpMethod::Post), MethodTone::Write);
    assert_eq!(method_tone(&HttpMethod::Put), MethodTone::Write);
    assert_eq!(method_tone(&HttpMethod::Patch), MethodTone::Write);
    assert_eq!(method_tone(&HttpMethod::Delete), MethodTone::Destructive);
    assert_eq!(method_tone(&HttpMethod::Connect), MethodTone::Other);
    assert_eq!(
        method_tone(&HttpMethod::Other("PURGE".into())),
        MethodTone::Other
    );
}

#[test]
fn fallback_text_omits_the_verb_when_empty() {
    assert_eq!(fallback_text("ssh", "", "user@host"), "ssh  user@host");
    assert_eq!(
        fallback_text("ssh", "ssh", "user@host"),
        "ssh  ssh user@host"
    );
}

#[test]
fn fallback_text_sanitises_every_component() {
    // The fallback row is the one card slot fed directly from
    // `PromptSummary` strings, so it is the most exposed to argv
    // bytes. None of the three components may pass a control
    // character through.
    let out = fallback_text("cu\u{202e}rl", "GE\u{0007}T", "https://x.test/\u{202e}");
    assert!(!out.contains('\u{202e}'), "{out:?}");
    assert!(!out.contains('\u{0007}'), "{out:?}");
}
