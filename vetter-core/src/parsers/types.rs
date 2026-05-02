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
#[path = "../tests/parsers_types.rs"]
mod tests;
