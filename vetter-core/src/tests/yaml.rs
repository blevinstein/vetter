//! Unit tests for [`crate::yaml`]: the parser options it pins and the
//! serde features the rule schema relies on.

use super::*;
use crate::matcher::rule::{HostPattern, Rule, RuleWhen};
use crate::HttpMethod;

#[test]
fn rejects_duplicate_keys() {
    let yaml = "id: a\nid: b\nwhen: { http: { method: [GET] } }\n";
    assert!(from_str::<Rule>(yaml).is_err());
}

#[test]
fn rejects_yaml_1_1_booleans() {
    let yaml = "id: a\nwhen: { http: { no_redirects: no } }\n";
    assert!(from_str::<Rule>(yaml).is_err());
}

#[test]
fn unknown_field_error_names_the_field() {
    let err = from_str::<Rule>("id: a\nwhen: {}\nbogus_field: 1\n").unwrap_err();
    assert!(err.to_string().contains("bogus_field"), "{err}");
}

#[test]
fn untagged_host_pattern_accepts_scalar_and_list() {
    assert_eq!(
        from_str::<HostPattern>("api.github.com").unwrap(),
        HostPattern::One("api.github.com".into())
    );
    assert_eq!(
        from_str::<HostPattern>("['*.github.com', github.com]").unwrap(),
        HostPattern::Many(vec!["*.github.com".into(), "github.com".into()])
    );
}

#[test]
fn method_strings_parse() {
    let when: RuleWhen = from_str("http: { method: [GET, DELETE] }").unwrap();
    assert_eq!(
        when.http.unwrap().method.unwrap(),
        vec![HttpMethod::Get, HttpMethod::Delete]
    );
}

#[test]
fn scalar_lookalike_strings_round_trip() {
    let when: RuleWhen = from_str("http: { url: { host: 'on', path: '0123' } }").unwrap();
    let out = to_string(&when).unwrap();
    assert_eq!(from_str::<RuleWhen>(&out).unwrap(), when, "{out}");
}
