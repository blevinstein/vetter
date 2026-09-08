//! `parsed.effects` → the rows beneath a card's URL line.
//!
//! [`effects`](super::effects) answers "what does this one effect
//! *say*" — the body meta label, the auth text, the form field line.
//! This module answers the questions above that: which effects earn a
//! row at all, what order the rows come in, and which of them belong
//! in the always-visible bucket.
//!
//! Both approval surfaces need the same answers. The macOS popover
//! keeps file-input rows inline on a resolved card while hiding the
//! rest behind a "▸ Details" disclosure; a surface that flattened the
//! two buckets could not express that. So [`effect_rows`] returns
//! them separately and [`EffectRows::flattened`] is the one-liner for
//! surfaces that paint everything inline.
//!
//! See [plans/ApprovalUI.md "Effect rows"](../../../plans/ApprovalUI.md)
//! for the token table each row renders to.

use std::path::{Path, PathBuf};

use vetter_core::render::sanitize_for_display;
use vetter_core::{Auth, Body, Effect, ParsedCommand};

use super::effects;

/// A file path plus whether the surface should offer to open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePath {
    pub display: String,
    pub path: PathBuf,
    /// False when the path does not exist, which suppresses the
    /// `Open file` button. Offering to open a file that is not there
    /// produces a confusing no-op, and for a `FileWrite` the file
    /// usually does not exist *yet* — which is exactly the case this
    /// suppresses.
    pub can_open: bool,
}

/// Body cell content, already lowered to strings by
/// [`super::effects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyContent {
    Inline(String),
    FromFile(FilePath),
    Form(Vec<String>),
}

/// One row beneath the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectRow {
    /// Header **names** only. Values never reach the card: agents
    /// routinely send bearer tokens, tenant ids and signed URLs in
    /// headers we cannot reliably classify, so the card treats every
    /// value as sensitive. The §8.5 raw body applies the
    /// `••••<last-4>` recipe if a user really wants to look.
    Headers(Vec<String>),
    Body {
        meta: String,
        content: BodyContent,
    },
    Auth {
        text: String,
        /// True when the credential was kept off-screen. Drives a
        /// *positive* tone, not an alarm — see
        /// [`super::effects::auth_label`].
        redacted: bool,
    },
    FileRead(FilePath),
    FileWrite(FilePath),
    ProcessSpawn(String),
}

/// Rows split by visibility class.
///
/// The split exists for resolved cards. A file the user uploaded is
/// the thing they are most likely to want to re-open after the fact,
/// so those rows stay inline; headers, body and auth are request
/// metadata that can fall behind a disclosure once the decision is
/// made. A surface that shows everything inline — as a pending card
/// does on both platforms — just calls [`Self::flattened`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectRows {
    /// `Effect::FileRead` plus the `Body::FromFile` branch.
    pub file_inputs: Vec<EffectRow>,
    /// Headers, non-file body, auth, `FileWrite`, `ProcessSpawn`.
    pub others: Vec<EffectRow>,
}

impl EffectRows {
    /// File inputs first, then everything else in source order.
    ///
    /// The "what file are you uploading?" question belongs at the top
    /// of the card; within `others`, source order keeps a single
    /// card's headers / body / auth sequence matching the §8.5
    /// renderer.
    pub fn flattened(self) -> Vec<EffectRow> {
        let mut out = self.file_inputs;
        out.extend(self.others);
        out
    }

    pub fn total(&self) -> usize {
        self.file_inputs.len() + self.others.len()
    }

    pub fn is_empty(&self) -> bool {
        self.file_inputs.is_empty() && self.others.is_empty()
    }
}

/// Lower `parsed.effects` into rows.
///
/// `FileRead`s whose path is already shown by a `Body::FromFile` row
/// are dropped: the curl parser deliberately emits both for
/// `-d @file` so the matcher can reason about the read independently
/// of the HTTP body, but a card rendering both would show the same
/// path twice. Only the visible row collapses — the effect list the
/// matcher sees is untouched.
///
/// `CredentialUse` and `Network` earn no row, as they get none in the
/// §8.5 layout either; "Show raw" still surfaces them.
pub fn effect_rows(parsed: &ParsedCommand) -> EffectRows {
    let body_files = effects::collect_body_file_paths(parsed);
    let mut out = EffectRows::default();

    for eff in &parsed.effects {
        match eff {
            Effect::HttpRequest(req) => {
                if !req.headers.is_empty() {
                    out.others.push(EffectRow::Headers(
                        req.headers
                            .iter()
                            .map(|h| sanitize_for_display(&h.name).into_owned())
                            .collect(),
                    ));
                }
                if let Some(meta) = effects::body_meta_label(&req.body) {
                    let content = match &req.body {
                        Body::Inline { bytes } => {
                            Some(BodyContent::Inline(effects::inline_body_text(bytes)))
                        }
                        Body::FromFile { path } => Some(BodyContent::FromFile(file_path(path))),
                        Body::Form { fields } => Some(BodyContent::Form(
                            fields.iter().map(effects::form_field_text).collect(),
                        )),
                        Body::None => None,
                    };
                    if let Some(content) = content {
                        // `Body::FromFile` is the only file-input
                        // shape of the body row; Inline and Form are
                        // request metadata like the rest.
                        let row = EffectRow::Body { meta, content };
                        if matches!(req.body, Body::FromFile { .. }) {
                            out.file_inputs.push(row);
                        } else {
                            out.others.push(row);
                        }
                    }
                }
                if let Some(auth) = req.auth.as_ref() {
                    let (text, redacted) = auth_row(auth);
                    out.others.push(EffectRow::Auth { text, redacted });
                }
            }
            Effect::FileRead(read) => {
                if !body_files.contains(&read.path) {
                    out.file_inputs
                        .push(EffectRow::FileRead(file_path(&read.path)));
                }
            }
            Effect::FileWrite(write) => {
                out.others
                    .push(EffectRow::FileWrite(file_path(&write.path)));
            }
            Effect::ProcessSpawn(spawn) => {
                out.others.push(EffectRow::ProcessSpawn(
                    sanitize_for_display(&spawn.command).into_owned(),
                ));
            }
            Effect::CredentialUse(_) | Effect::Network(_) => {}
        }
    }

    out
}

/// The auth row's text and whether the credential was withheld.
pub fn auth_row(auth: &Auth) -> (String, bool) {
    effects::auth_label(auth)
}

/// Lower a path into its display string plus the `Open file`
/// suppression answer.
pub fn file_path(path: &Path) -> FilePath {
    FilePath {
        display: sanitize_for_display(&path.display().to_string()).into_owned(),
        path: path.to_path_buf(),
        can_open: can_open(path),
    }
}

/// Whether a surface should offer an `Open file` button for `path`.
///
/// Existence is the whole rule, and it is answered at card-build time
/// rather than at click time so the button is absent rather than
/// present-and-broken.
pub fn can_open(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
#[path = "../tests/cards_rows.rs"]
mod tests;
