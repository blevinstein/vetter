//! Tests for [`crate::parsers::curl::flags`]. Layout convention is
//! described in `AGENTS.md`.

use super::*;

fn s(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn tokens(args: &[&str]) -> Vec<Token> {
    tokenise(&s(args)).expect("tokenise")
}

#[test]
fn lookup_short_and_long_resolve_same_flag() {
    let by_short = lookup_short('k').expect("k");
    let by_long = lookup_long("insecure").expect("insecure");
    assert_eq!(by_short.id, by_long.id);
    assert_eq!(by_short.id, FlagId::Insecure);
}

#[test]
fn positional_url() {
    let toks = tokens(&["https://example.test/"]);
    assert_eq!(
        toks,
        vec![Token::Positional("https://example.test/".into())]
    );
}

#[test]
fn long_value_flag_with_separate_arg() {
    let toks = tokens(&["--request", "POST"]);
    assert_eq!(
        toks,
        vec![Token::Known {
            spec: lookup_long("request").unwrap(),
            value: Some("POST".into()),
        }]
    );
}

#[test]
fn long_value_flag_with_equals() {
    let toks = tokens(&["--request=POST"]);
    assert_eq!(
        toks,
        vec![Token::Known {
            spec: lookup_long("request").unwrap(),
            value: Some("POST".into()),
        }]
    );
}

#[test]
fn short_value_flag_with_smashed_value() {
    let toks = tokens(&["-XPOST"]);
    assert_eq!(
        toks,
        vec![Token::Known {
            spec: lookup_short('X').unwrap(),
            value: Some("POST".into()),
        }]
    );
}

#[test]
fn short_bool_cluster() {
    let toks = tokens(&["-kL"]);
    assert_eq!(
        toks,
        vec![
            Token::Known {
                spec: lookup_short('k').unwrap(),
                value: None
            },
            Token::Known {
                spec: lookup_short('L').unwrap(),
                value: None
            },
        ]
    );
}

#[test]
fn unknown_long_flag_preserved() {
    let toks = tokens(&["--never-heard-of-it"]);
    assert_eq!(
        toks,
        vec![Token::UnknownLong {
            name: "never-heard-of-it".into(),
            value: None,
        }]
    );
}

#[test]
fn unknown_long_flag_with_equals_value_preserved() {
    let toks = tokens(&["--mystery=42"]);
    assert_eq!(
        toks,
        vec![Token::UnknownLong {
            name: "mystery".into(),
            value: Some("42".into()),
        }]
    );
}

#[test]
fn missing_value_for_long_value_flag_errors() {
    let r = tokenise(&s(&["--request"]));
    assert!(matches!(r, Err(ParseError::MissingArgument(_))));
}

#[test]
fn missing_value_for_short_value_flag_errors() {
    let r = tokenise(&s(&["-X"]));
    assert!(matches!(r, Err(ParseError::MissingArgument(_))));
}

#[test]
fn dashdash_treats_remaining_as_positional() {
    let toks = tokens(&["--", "-X", "POST"]);
    assert_eq!(
        toks,
        vec![
            Token::Positional("-X".into()),
            Token::Positional("POST".into()),
        ]
    );
}

#[test]
fn bare_dash_is_positional() {
    let toks = tokens(&["-"]);
    assert_eq!(toks, vec![Token::Positional("-".into())]);
}
