//! Tests for [`crate::matcher::rule`]. Layout convention is described
//! in `AGENTS.md`.

use super::*;

fn roundtrip_yaml(yaml: &str) -> Rule {
    let rule: Rule = serde_yaml_ng::from_str(yaml).expect("yaml -> rule");
    let back = serde_yaml_ng::to_string(&rule).expect("rule -> yaml");
    let again: Rule = serde_yaml_ng::from_str(&back).expect("yaml -> rule (round 2)");
    assert_eq!(rule, again);
    rule
}

#[test]
fn rule_round_trip_full_http_clause() {
    let yaml = r#"
id: github-readonly
command: curl
when:
  http:
    method: [GET, HEAD]
    url:
      scheme: https
      host: api.github.com
      port: [443]
      path: "/repos/**"
    headers_allow: ["Accept", "User-Agent"]
    no_body: true
    query: false
note: read-only
"#;
    let r = roundtrip_yaml(yaml);
    assert_eq!(r.id, "github-readonly");
    assert_eq!(r.command.as_deref(), Some("curl"));
    let http = r.when.http.expect("http clause");
    assert_eq!(
        http.method.as_deref(),
        Some(&[HttpMethod::Get, HttpMethod::Head][..])
    );
    let url = http.url.expect("url");
    assert_eq!(url.scheme.as_deref(), Some("https"));
    assert!(matches!(url.host, Some(HostPattern::One(ref s)) if s == "api.github.com"));
    assert_eq!(url.port, Some(vec![443]));
    assert_eq!(url.path.as_deref(), Some("/repos/**"));
    assert_eq!(
        http.headers_allow.as_deref(),
        Some(&["Accept".to_string(), "User-Agent".to_string()][..])
    );
    assert_eq!(http.no_body, Some(true));
    assert_eq!(http.query, Some(false));
}

#[test]
fn host_pattern_accepts_string_or_list() {
    let one: HostPattern = serde_yaml_ng::from_str("api.github.com").unwrap();
    assert!(matches!(one, HostPattern::One(_)));
    let many: HostPattern = serde_yaml_ng::from_str("[\"*.github.com\", \"github.com\"]").unwrap();
    assert!(matches!(many, HostPattern::Many(ref v) if v.len() == 2));
    assert_eq!(many.patterns().len(), 2);
    assert_eq!(one.patterns(), &["api.github.com".to_string()]);
}

#[test]
fn rule_round_trip_file_write_only() {
    let yaml = r#"
id: tmp-only
when:
  file_write:
    path: "/tmp/**"
"#;
    let r = roundtrip_yaml(yaml);
    assert!(r.command.is_none());
    assert!(r.when.http.is_none());
    let fw = r.when.file_write.expect("file_write");
    assert_eq!(fw.path.as_deref(), Some("/tmp/**"));
}

#[test]
fn unknown_field_in_rule_rejected() {
    let yaml = r#"
id: nope
when: {}
made_up_field: 1
"#;
    let err = serde_yaml_ng::from_str::<Rule>(yaml).unwrap_err();
    assert!(
        err.to_string().contains("made_up_field") || err.to_string().contains("unknown field"),
        "{err}"
    );
}

#[test]
fn unknown_field_in_http_clause_rejected() {
    let yaml = r#"
id: nope
when:
  http:
    method: [GET]
    nonsense: 1
"#;
    let err = serde_yaml_ng::from_str::<Rule>(yaml).unwrap_err();
    assert!(err.to_string().contains("nonsense"), "{err}");
}

#[test]
fn rule_with_all_clauses_empty_can_still_deserialise() {
    let yaml = r#"
id: empty
when: {}
"#;
    let r: Rule = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(r.when.is_empty());
}

#[test]
fn rule_round_trip_no_redirects_opt_in() {
    // ThreatModel T10: rules that genuinely need redirect-following
    // trust must say so explicitly. Round-trip the opt-in form so a
    // future renaming of the field surfaces as a test failure.
    let yaml = r#"
id: redirect-friendly
when:
  http:
    method: [GET]
    no_redirects: false
"#;
    let r = roundtrip_yaml(yaml);
    let http = r.when.http.expect("http clause");
    assert_eq!(http.no_redirects, Some(false));
}

#[test]
fn rule_round_trip_no_redirects_omitted_is_none() {
    // Default-deny is encoded by the absence of `no_redirects` in
    // YAML; the field deserialises as `None` and is skipped on
    // re-serialisation.
    let yaml = r#"
id: strict
when:
  http:
    method: [GET]
"#;
    let r = roundtrip_yaml(yaml);
    let http = r.when.http.as_ref().expect("http clause");
    assert!(http.no_redirects.is_none());
    let back = serde_yaml_ng::to_string(&r).expect("rule -> yaml");
    assert!(
        !back.contains("no_redirects"),
        "omitted no_redirects should not be serialised: {back}",
    );
}
