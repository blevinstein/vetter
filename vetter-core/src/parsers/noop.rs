//! Trivial `CommandParser` used in tests and snapshots.
//!
//! Gated behind `cfg(any(test, feature = "test-parsers"))` so it never
//! ships in the release `vet` / `vetterd` binaries. Lets us exercise
//! the parser → analyzer → renderer pipeline without depending on the
//! real curl parser (Phase 1b).

use url::Url;

use super::{
    Body, CommandParser, DisplayHints, Effect, EnvSnapshot, Header, HttpMethod, HttpRequest,
    ParseError, ParsedCommand, StdinHandle, TlsPolicy,
};

/// `noop https://example.test/foo` → a single GET effect. If a second
/// argument is provided, it overrides the URL.
pub struct NoopParser;

impl CommandParser for NoopParser {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn handles(&self, argv0: &str) -> bool {
        argv0 == "noop"
    }

    fn parse(
        &self,
        argv: &[String],
        _stdin: StdinHandle<'_>,
        _env: &EnvSnapshot,
    ) -> Result<ParsedCommand, ParseError> {
        let url_str = argv
            .get(1)
            .map(String::as_str)
            .unwrap_or("https://example.test/");
        let url = Url::parse(url_str)
            .map_err(|e| ParseError::Other(format!("bad url `{url_str}`: {e}")))?;

        let req = HttpRequest {
            method: HttpMethod::Get,
            url: url.clone(),
            headers: vec![Header {
                name: "User-Agent".into(),
                value: "vet-noop/0".into(),
            }],
            body: Body::None,
            auth: None,
            tls: TlsPolicy::Strict,
            follow_redirects: false,
            proxy: None,
        };

        Ok(ParsedCommand {
            command: "noop".into(),
            argv: argv.to_vec(),
            cwd: None,
            effects: vec![Effect::HttpRequest(req)],
            signals: vec![],
            display_hints: DisplayHints {
                primary_verb: "GET".into(),
                primary_target: url.to_string(),
                badges: vec![],
            },
            extras: serde_json::Value::Null,
        })
    }
}
