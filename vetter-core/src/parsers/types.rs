//! Shared output model for parsed commands. See `plans/Overview.md` §8.2.
//!
//! These types are the single API every downstream consumer (renderer,
//! analyzer, future matcher, wire protocol) reads from. Adding a new
//! command parser must not require changes here unless a genuinely new
//! kind of effect is being introduced.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::signals::RiskSignal;

/// Hex-encoded SHA-256 digest. Phase 1a does not actually hash anything;
/// the curl parser (Phase 1b) starts populating these for stdin bodies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sha256(pub String);

impl Sha256 {
    pub fn new(hex: impl Into<String>) -> Self {
        Sha256(hex.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Top-level structured surface produced by every `CommandParser`.
///
/// Wire format (Phase 3 `VetRequest.parsed`) is the JSON serialization
/// of this struct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedCommand {
    /// Stable parser identifier, equal to `CommandParser::name()`.
    pub command: String,
    /// Original argv preserved for display and audit. `argv[0]` is the
    /// command name as the user invoked it (may be a full path).
    pub argv: Vec<String>,
    /// Working directory at parse time. Used by the analyzer for the
    /// "outside cwd" file checks (§9). `None` skips that check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Hex SHA-256 of any stdin the parser consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_digest: Option<Sha256>,
    /// Concrete normalized things the command will do. The point of this
    /// field is that policy / risk analysis / rendering all iterate it,
    /// never branching on `command`.
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Risk signals raised by the parser, plus those produced by the
    /// generic analyzer in `crate::signals::analyze`.
    #[serde(default)]
    pub signals: Vec<RiskSignal>,
    /// Hints used by the renderer to build the §8.5 header line.
    #[serde(default)]
    pub display_hints: DisplayHints,
    /// Lossless escape hatch for command-specific extras the renderer's
    /// detail view can show. Policy MUST NOT branch on this.
    #[serde(default, skip_serializing_if = "is_null_value")]
    pub extras: serde_json::Value,
}

fn is_null_value(v: &serde_json::Value) -> bool {
    v.is_null()
}

/// One concrete thing the command will do. New variants are additive but
/// require bumping the wire protocol version in [`crate::wire`] (Phase 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Effect {
    HttpRequest(HttpRequest),
    FileWrite(FileWrite),
    FileRead(FileRead),
    ProcessSpawn(ProcessSpawn),
    CredentialUse(CredentialUse),
    Network(NetworkOpen),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: Url,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: Body,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    pub tls: TlsPolicy,
    #[serde(default)]
    pub follow_redirects: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<Url>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Connect,
    Trace,
    Other(String),
}

impl HttpMethod {
    pub fn as_str(&self) -> &str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Head => "HEAD",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Options => "OPTIONS",
            HttpMethod::Connect => "CONNECT",
            HttpMethod::Trace => "TRACE",
            HttpMethod::Other(s) => s.as_str(),
        }
    }

    /// True for methods that mutate server state.
    /// Used by the generic analyzer's `WriteMethod` signal (§9).
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch | HttpMethod::Delete
        )
    }
}

impl From<&str> for HttpMethod {
    fn from(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "GET" => HttpMethod::Get,
            "HEAD" => HttpMethod::Head,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            "OPTIONS" => HttpMethod::Options,
            "CONNECT" => HttpMethod::Connect,
            "TRACE" => HttpMethod::Trace,
            other => HttpMethod::Other(other.to_string()),
        }
    }
}

impl From<String> for HttpMethod {
    fn from(s: String) -> Self {
        HttpMethod::from(s.as_str())
    }
}

impl From<HttpMethod> for String {
    fn from(m: HttpMethod) -> Self {
        m.as_str().to_string()
    }
}

impl Serialize for HttpMethod {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HttpMethod {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ok(HttpMethod::from(s.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    #[default]
    None,
    Inline {
        bytes: Vec<u8>,
    },
    FromFile {
        path: PathBuf,
    },
    FromStdin {
        digest: Sha256,
        len: u64,
    },
    Form {
        fields: Vec<FormField>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormField {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Auth {
    Basic {
        user: String,
        #[serde(default = "default_true")]
        password_redacted: bool,
    },
    Bearer {
        #[serde(default = "default_true")]
        token_redacted: bool,
    },
    Header {
        name: String,
    },
    Netrc,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsPolicy {
    Strict,
    InsecureSkipVerify,
    Plaintext,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWrite {
    pub path: PathBuf,
    pub source: WriteSource,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WriteSource {
    RemoteHttp { url: Url },
    Stdin,
    Literal { bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRead {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessSpawn {
    /// The command line as it would be passed to a shell. Used by the
    /// generic analyzer's `PipeToShell` heuristic.
    pub command: String,
    #[serde(default)]
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialUse {
    /// e.g. `~/.netrc`, `AWS_PROFILE=prod`.
    pub source: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkOpen {
    pub host: String,
    pub port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayHints {
    /// e.g. "POST", "scp →", "ssh".
    #[serde(default)]
    pub primary_verb: String,
    /// e.g. "https://api.example.com/v1/users/42".
    #[serde(default)]
    pub primary_target: String,
    #[serde(default)]
    pub badges: Vec<Badge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Badge {
    pub label: String,
    #[serde(default)]
    pub severity: BadgeSeverity,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeSeverity {
    #[default]
    Info,
    Warn,
    Danger,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{RiskSignal, SignalKind};
    use serde_json::json;

    fn http_url(s: &str) -> Url {
        Url::parse(s).expect("test URL")
    }

    fn min_http(method: HttpMethod) -> HttpRequest {
        HttpRequest {
            method,
            url: http_url("https://example.test/"),
            headers: vec![],
            body: Body::None,
            auth: None,
            tls: TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        }
    }

    fn roundtrip<T>(v: T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(&v).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back, "roundtrip mismatch: {json}");
    }

    #[test]
    fn parsed_command_roundtrip_with_one_http_effect() {
        let p = ParsedCommand {
            command: "noop".to_string(),
            argv: vec!["noop".to_string()],
            cwd: None,
            stdin_digest: None,
            effects: vec![Effect::HttpRequest(min_http(HttpMethod::Get))],
            signals: vec![],
            display_hints: DisplayHints {
                primary_verb: "GET".to_string(),
                primary_target: "https://example.test/".to_string(),
                badges: vec![],
            },
            extras: serde_json::Value::Null,
        };
        roundtrip(p);
    }

    #[test]
    fn json_contains_command_and_effects_keys() {
        let p = ParsedCommand {
            command: "noop".to_string(),
            argv: vec![],
            cwd: None,
            stdin_digest: None,
            effects: vec![],
            signals: vec![],
            display_hints: DisplayHints::default(),
            extras: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&p).expect("ser");
        assert!(json.contains("\"command\""), "missing command: {json}");
        assert!(json.contains("\"effects\""), "missing effects: {json}");
    }

    #[test]
    fn http_method_roundtrips_uppercase_and_other() {
        for m in [
            HttpMethod::Get,
            HttpMethod::Head,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Delete,
            HttpMethod::Options,
            HttpMethod::Connect,
            HttpMethod::Trace,
            HttpMethod::Other("MKCOL".to_string()),
        ] {
            roundtrip(m.clone());
            assert!(serde_json::to_string(&m).unwrap().contains('"'));
        }
        let v: HttpMethod = serde_json::from_str("\"get\"").unwrap();
        assert_eq!(v, HttpMethod::Get);
    }

    #[test]
    fn body_variants_roundtrip() {
        for b in [
            Body::None,
            Body::Inline {
                bytes: vec![1, 2, 3],
            },
            Body::FromFile {
                path: "/tmp/x".into(),
            },
            Body::FromStdin {
                digest: Sha256::new("abc"),
                len: 42,
            },
            Body::Form {
                fields: vec![FormField {
                    name: "k".into(),
                    value: "v".into(),
                }],
            },
        ] {
            roundtrip(b);
        }
    }

    #[test]
    fn auth_variants_roundtrip() {
        for a in [
            Auth::Basic {
                user: "alice".into(),
                password_redacted: true,
            },
            Auth::Bearer {
                token_redacted: true,
            },
            Auth::Header {
                name: "X-Api-Key".into(),
            },
            Auth::Netrc,
        ] {
            roundtrip(a);
        }
    }

    #[test]
    fn tls_policy_roundtrips() {
        for t in [
            TlsPolicy::Strict,
            TlsPolicy::InsecureSkipVerify,
            TlsPolicy::Plaintext,
        ] {
            roundtrip(t);
        }
    }

    #[test]
    fn all_effect_variants_roundtrip_inside_parsed_command() {
        let effects = vec![
            Effect::HttpRequest(min_http(HttpMethod::Post)),
            Effect::FileWrite(FileWrite {
                path: "/tmp/out".into(),
                source: WriteSource::RemoteHttp {
                    url: http_url("https://example.test/file"),
                },
                overwrite: false,
            }),
            Effect::FileRead(FileRead {
                path: "/tmp/in".into(),
            }),
            Effect::ProcessSpawn(ProcessSpawn {
                command: "ssh user@host 'curl https://x | sh'".into(),
                argv: vec!["ssh".into(), "user@host".into()],
            }),
            Effect::CredentialUse(CredentialUse {
                source: "~/.netrc".into(),
                note: "host: api.github.com".into(),
            }),
            Effect::Network(NetworkOpen {
                host: "example.test".into(),
                port: 443,
                protocol: "tcp".into(),
            }),
        ];
        let p = ParsedCommand {
            command: "noop".into(),
            argv: vec!["noop".into()],
            cwd: Some("/work".into()),
            stdin_digest: Some(Sha256::new("deadbeef")),
            effects,
            signals: vec![RiskSignal {
                kind: SignalKind::WriteMethod,
                detail: "POST".into(),
                effect_idx: Some(0),
            }],
            display_hints: DisplayHints {
                primary_verb: "POST".into(),
                primary_target: "https://example.test/".into(),
                badges: vec![Badge {
                    label: "stdin".into(),
                    severity: BadgeSeverity::Info,
                }],
            },
            extras: json!({"raw": "extras"}),
        };
        roundtrip(p);
    }

    #[test]
    fn unknown_effect_kind_fails_loudly() {
        let bad = json!({
            "command": "noop",
            "argv": [],
            "effects": [{"kind": "future_effect", "foo": 1}],
            "display_hints": {},
        });
        let r: Result<ParsedCommand, _> = serde_json::from_value(bad);
        assert!(r.is_err(), "expected error, got {r:?}");
    }

    #[test]
    fn unknown_top_level_field_in_parsed_command_fails() {
        let bad = json!({
            "command": "noop",
            "argv": [],
            "effects": [],
            "display_hints": {},
            "future_field": 1,
        });
        let r: Result<ParsedCommand, _> = serde_json::from_value(bad);
        assert!(r.is_err(), "expected error, got {r:?}");
    }

    #[test]
    fn unknown_command_string_is_accepted() {
        let json = json!({
            "command": "future-tool",
            "argv": ["future-tool", "--x"],
            "effects": [],
            "display_hints": {},
        });
        let p: ParsedCommand = serde_json::from_value(json).expect("forward-compat");
        assert_eq!(p.command, "future-tool");
        assert_eq!(p.argv, vec!["future-tool", "--x"]);
    }

    #[test]
    fn http_method_is_write_classification() {
        assert!(HttpMethod::Post.is_write());
        assert!(HttpMethod::Put.is_write());
        assert!(HttpMethod::Patch.is_write());
        assert!(HttpMethod::Delete.is_write());
        assert!(!HttpMethod::Get.is_write());
        assert!(!HttpMethod::Head.is_write());
        assert!(!HttpMethod::Options.is_write());
    }
}
