//! Curl flag table + argv tokeniser.
//!
//! This module is the data half of the curl parser: a static description
//! of the flags we recognise, plus a tokeniser that walks an argv slice
//! and emits one [`Token`] per flag/positional. Collection and
//! finalisation live in [`super::state`].
//!
//! Curl's real flag set is enormous; we cover the subset exercised by
//! the corpus in `vetter-core/tests/corpus/curl/`. Unknown long flags
//! are preserved (not errored) per `plans/TestingPlan.md` §3.1; unknown
//! short flags are also preserved as bool-like extras since we cannot
//! know whether they take a value.

use crate::parsers::ParseError;

/// Whether a flag consumes a value and how many times it may appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKind {
    /// No value; presence-only.
    Bool,
    /// Takes one value; later occurrences override earlier ones.
    Value,
    /// Takes one value per occurrence; all are kept in order.
    MultiValue,
}

/// Stable identifier for every flag we recognise. The `state` module
/// does an exhaustive `match` over this so adding a variant forces an
/// update there too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlagId {
    Request,
    Header,
    Data,
    DataRaw,
    DataBinary,
    DataAscii,
    DataUrlencode,
    User,
    Insecure,
    Output,
    RemoteName,
    RemoteHeaderName,
    UploadFile,
    Location,
    Proxy,
    UserAgent,
    Netrc,
    CaCert,
    Resolve,
    UnixSocket,
    Get,
    Head,
    Silent,
    Verbose,
    Fail,
    ShowError,
    ProgressBar,
    Config,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagSpec {
    pub id: FlagId,
    pub short: Option<char>,
    pub long: Option<&'static str>,
    pub kind: FlagKind,
}

/// The recognised flag set. Order is informational only; lookup is
/// linear and the table is small enough that hashing isn't worth it.
pub const FLAG_SPECS: &[FlagSpec] = &[
    FlagSpec {
        id: FlagId::Request,
        short: Some('X'),
        long: Some("request"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::Header,
        short: Some('H'),
        long: Some("header"),
        kind: FlagKind::MultiValue,
    },
    FlagSpec {
        id: FlagId::Data,
        short: Some('d'),
        long: Some("data"),
        kind: FlagKind::MultiValue,
    },
    FlagSpec {
        id: FlagId::DataRaw,
        short: None,
        long: Some("data-raw"),
        kind: FlagKind::MultiValue,
    },
    FlagSpec {
        id: FlagId::DataBinary,
        short: None,
        long: Some("data-binary"),
        kind: FlagKind::MultiValue,
    },
    FlagSpec {
        id: FlagId::DataAscii,
        short: None,
        long: Some("data-ascii"),
        kind: FlagKind::MultiValue,
    },
    FlagSpec {
        id: FlagId::DataUrlencode,
        short: None,
        long: Some("data-urlencode"),
        kind: FlagKind::MultiValue,
    },
    FlagSpec {
        id: FlagId::User,
        short: Some('u'),
        long: Some("user"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::Insecure,
        short: Some('k'),
        long: Some("insecure"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::Output,
        short: Some('o'),
        long: Some("output"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::RemoteName,
        short: Some('O'),
        long: Some("remote-name"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::RemoteHeaderName,
        short: Some('J'),
        long: Some("remote-header-name"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::UploadFile,
        short: Some('T'),
        long: Some("upload-file"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::Location,
        short: Some('L'),
        long: Some("location"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::Proxy,
        short: Some('x'),
        long: Some("proxy"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::UserAgent,
        short: Some('A'),
        long: Some("user-agent"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::Netrc,
        short: None,
        long: Some("netrc"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::CaCert,
        short: None,
        long: Some("cacert"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::Resolve,
        short: None,
        long: Some("resolve"),
        kind: FlagKind::MultiValue,
    },
    FlagSpec {
        id: FlagId::UnixSocket,
        short: None,
        long: Some("unix-socket"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::Get,
        short: Some('G'),
        long: Some("get"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::Head,
        short: Some('I'),
        long: Some("head"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::Silent,
        short: Some('s'),
        long: Some("silent"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::Verbose,
        short: Some('v'),
        long: Some("verbose"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::Fail,
        short: Some('f'),
        long: Some("fail"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::ShowError,
        short: Some('S'),
        long: Some("show-error"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::ProgressBar,
        short: Some('#'),
        long: Some("progress-bar"),
        kind: FlagKind::Bool,
    },
    FlagSpec {
        id: FlagId::Config,
        short: Some('K'),
        long: Some("config"),
        kind: FlagKind::Value,
    },
    FlagSpec {
        id: FlagId::Next,
        short: Some(':'),
        long: Some("next"),
        kind: FlagKind::Bool,
    },
];

pub fn lookup_long(name: &str) -> Option<&'static FlagSpec> {
    FLAG_SPECS.iter().find(|s| s.long == Some(name))
}

pub fn lookup_short(c: char) -> Option<&'static FlagSpec> {
    FLAG_SPECS.iter().find(|s| s.short == Some(c))
}

/// One token emitted by [`tokenise`]. Captures everything the state
/// collector needs to know about a single argv slot (or pair of slots,
/// when a value flag consumed the next slot for its value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Known {
        spec: &'static FlagSpec,
        /// `Some` for Value/MultiValue flags; `None` for Bool flags.
        value: Option<String>,
    },
    /// `--unknown-flag` or `--unknown-flag=value`. Preserved so the
    /// renderer's detail view (via `extras`) can show it; never
    /// auto-allowed against because the matcher ignores `extras`.
    UnknownLong { name: String, value: Option<String> },
    /// `-q` where `q` is not in the flag table. We don't know whether
    /// it takes a value, so we never consume the next argv token.
    UnknownShort { c: char },
    /// Anything not starting with `-`, plus everything after `--`.
    Positional(String),
}

/// Walk an argv slice (already with `argv[0]` stripped) and emit one
/// [`Token`] per flag / positional. The only error path is a Value /
/// MultiValue flag with no following argv slot.
pub fn tokenise(argv: &[String]) -> Result<Vec<Token>, ParseError> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    let mut after_dashdash = false;
    while i < argv.len() {
        let tok = &argv[i];
        if after_dashdash {
            out.push(Token::Positional(tok.clone()));
            i += 1;
            continue;
        }
        if tok == "--" {
            after_dashdash = true;
            i += 1;
            continue;
        }
        if let Some(rest) = tok.strip_prefix("--") {
            let (name, embedded) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            match lookup_long(name) {
                Some(spec) => match spec.kind {
                    FlagKind::Bool => {
                        if embedded.is_some() {
                            return Err(ParseError::Other(format!(
                                "flag --{name} does not take a value"
                            )));
                        }
                        out.push(Token::Known { spec, value: None });
                    }
                    FlagKind::Value | FlagKind::MultiValue => {
                        let value = match embedded {
                            Some(v) => v,
                            None => {
                                i += 1;
                                argv.get(i).cloned().ok_or_else(|| {
                                    ParseError::MissingArgument(format!("--{name}"))
                                })?
                            }
                        };
                        out.push(Token::Known {
                            spec,
                            value: Some(value),
                        });
                    }
                },
                None => {
                    out.push(Token::UnknownLong {
                        name: name.to_string(),
                        value: embedded,
                    });
                }
            }
            i += 1;
            continue;
        }
        if let Some(rest) = tok.strip_prefix('-') {
            // Bare `-` is a positional (curl uses it for stdin).
            if rest.is_empty() {
                out.push(Token::Positional(tok.clone()));
                i += 1;
                continue;
            }
            // `-Xfoo` or `-X foo` or `-kL` cluster.
            let mut chars = rest.chars();
            let first = chars.next().expect("non-empty");
            let tail: String = chars.collect();
            match lookup_short(first) {
                Some(spec) => match spec.kind {
                    FlagKind::Bool => {
                        out.push(Token::Known { spec, value: None });
                        // Remaining chars are clustered bool flags.
                        for c in tail.chars() {
                            match lookup_short(c) {
                                Some(s) if s.kind == FlagKind::Bool => {
                                    out.push(Token::Known {
                                        spec: s,
                                        value: None,
                                    });
                                }
                                Some(_) => {
                                    return Err(ParseError::Other(format!(
                                        "value-taking short flag -{c} cannot appear inside a -{first}{tail} cluster"
                                    )));
                                }
                                None => {
                                    out.push(Token::UnknownShort { c });
                                }
                            }
                        }
                    }
                    FlagKind::Value | FlagKind::MultiValue => {
                        let value = if !tail.is_empty() {
                            tail
                        } else {
                            i += 1;
                            argv.get(i)
                                .cloned()
                                .ok_or_else(|| ParseError::MissingArgument(format!("-{first}")))?
                        };
                        out.push(Token::Known {
                            spec,
                            value: Some(value),
                        });
                    }
                },
                None => {
                    out.push(Token::UnknownShort { c: first });
                    // Conservatively treat any tail chars as further
                    // unknown shorts so we don't silently swallow them.
                    for c in tail.chars() {
                        out.push(Token::UnknownShort { c });
                    }
                }
            }
            i += 1;
            continue;
        }
        out.push(Token::Positional(tok.clone()));
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
#[path = "../../tests/parsers_curl_flags.rs"]
mod tests;
