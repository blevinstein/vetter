//! One pending request, lowered to the strings and tones a surface
//! paints.
//!
//! This is the aggregation layer: [`super::url`], [`super::pills`],
//! [`super::rows`] and [`super::spans`] each answer one question, and
//! [`card_view`] assembles their answers into the shape a card is
//! built from. Keeping the assembly here rather than in each
//! platform's widget code is what stops the two surfaces disagreeing
//! about which rows exist or which pill sorts first.
//!
//! Nothing here names a colour or a toolkit type. Colour *choices*
//! are carried as semantic tones ([`super::url::MethodTone`],
//! [`super::url::HostTrust`], [`super::pills::Tone`]) and resolved
//! against a palette by whichever surface is painting.

use vetter_core::render::sanitize_for_display;
use vetter_core::Effect;

use super::pills::PillSpec;
use super::rows::EffectRow;
use super::{pills, rows, spans, url};
use crate::pending::PromptSummary;

/// The URL row's typed tokens, or the fallback line for cards with no
/// HTTP effect.
///
/// See [plans/ApprovalUI.md "URL row"](../../../plans/ApprovalUI.md)
/// for the token-by-token style table; every decision about *which*
/// tokens appear is made in [`super::url`], not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlView {
    Http(HttpUrlView),
    /// `<command>  <verb> <target>`, for `ProcessSpawn`-only cards and
    /// for summaries that predate the `parsed` field. Keeps the slot
    /// populated so the card's vertical cadence is constant.
    Fallback(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrlView {
    pub method: String,
    pub method_tone: url::MethodTone,
    /// `https://`, including the separator.
    pub scheme: String,
    pub host: String,
    pub trust: url::HostTrust,
    /// Only present when the port is worth showing at all — see
    /// [`super::url::visible_port`].
    pub port: Option<u16>,
    pub tail: url::PathQuery,
}

/// The "Show raw" disclosure's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawView {
    /// Styled runs, for painting.
    pub spans: Vec<spans::AnsiSpan>,
    /// The same text with SGR escapes stripped, for the clipboard.
    /// Kept separate so a copy button writes exactly the bytes the
    /// user can see rather than anything a markup layer added.
    pub plain: String,
}

/// One pending request, lowered for painting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardView {
    /// Correlation ULID: the identity buttons resolve against, and
    /// the key a surface tracks per-card state by.
    pub id: String,
    /// Title line, e.g. `curl`.
    pub title: String,
    /// True when `vet --dry-run` forced this onto the prompt path.
    /// Drives the dry-run wrapper rather than an inline pill, so the
    /// two classes of card read apart at a glance when both are in
    /// the list.
    pub dry_run: bool,
    pub url: UrlView,
    /// Signal pills, deduped by kind and sorted most-urgent-first.
    pub pills: Vec<PillSpec>,
    /// Per-effect rows, split by visibility class.
    pub rows: rows::EffectRows,
    /// The §8.5 raw body behind the "Show raw" disclosure.
    pub raw: RawView,
    /// Host-trust for the primary host, also carried on
    /// [`HttpUrlView`]. Kept at card level so a fallback card can
    /// still colour its chrome.
    pub trust: url::HostTrust,
    /// Whether the `Trust host…` button belongs on this card. See
    /// [`super::picker::show_trust_host`].
    pub show_trust_host: bool,
}

impl CardView {
    /// Every row, file inputs first — for surfaces that paint the
    /// whole set inline rather than hiding metadata behind a
    /// disclosure.
    pub fn all_rows(&self) -> Vec<EffectRow> {
        self.rows.clone().flattened()
    }
}

/// Lower one summary plus its pre-rendered §8.5 detail into a card.
///
/// Every argv-derived string goes through
/// [`vetter_core::render::sanitize_for_display`] — mostly inside the
/// sibling modules, which sanitise at the point each string is built.
/// A hostile URL carrying RTLO or zero-width bytes would otherwise be
/// painted verbatim onto the surface a human uses to authorise it.
/// Escaping for markup is a *separate* concern applied at the point
/// text enters a markup-bearing widget.
pub fn card_view(summary: &PromptSummary, rendered: &str) -> CardView {
    let title = sanitize_for_display(&summary.command).into_owned();
    let trust = trust_of(summary);

    CardView {
        id: summary.id.clone(),
        title,
        dry_run: summary.force_prompt,
        url: url_view(summary, trust),
        pills: pills::pills_for(&summary.signals),
        rows: summary
            .parsed
            .as_ref()
            .map(rows::effect_rows)
            .unwrap_or_default(),
        raw: RawView {
            spans: spans::parse_ansi_spans(rendered),
            plain: spans::strip_ansi(rendered),
        },
        trust,
        show_trust_host: super::picker::show_trust_host(&summary.signals),
    }
}

/// The URL row for this summary, or the fallback line.
pub fn url_view(summary: &PromptSummary, trust: url::HostTrust) -> UrlView {
    let req = summary
        .parsed
        .as_ref()
        .and_then(|p| p.effects.first())
        .and_then(|e| match e {
            Effect::HttpRequest(req) => Some(req),
            _ => None,
        });

    match req {
        Some(req) => UrlView::Http(HttpUrlView {
            method: req.method.as_str().to_string(),
            method_tone: url::method_tone(&req.method),
            scheme: format!("{}://", req.url.scheme()),
            host: sanitize_for_display(req.url.host_str().unwrap_or_default()).into_owned(),
            trust,
            port: url::visible_port(&req.url),
            tail: url::path_query(&req.url),
        }),
        None => UrlView::Fallback(url::fallback_text(
            &summary.command,
            &summary.primary_verb,
            &summary.primary_target,
        )),
    }
}

/// Host-trust for the summary's primary host.
///
/// `host_known` is computed daemon-side against the known-hosts store
/// (a UI surface cannot reach it), so entry 0 — the primary HTTP
/// effect — is what the pill reflects. Falls back to "unknown" when
/// the summary predates that field or carries no HTTP effect, which
/// is the conservative direction: an unknown-host pill overstates
/// risk, a known-host pill would understate it.
pub fn trust_of(summary: &PromptSummary) -> url::HostTrust {
    let host = summary
        .parsed
        .as_ref()
        .and_then(|p| p.effects.first())
        .and_then(|e| match e {
            Effect::HttpRequest(req) => req.url.host_str(),
            _ => None,
        });
    match host {
        Some(h) => url::host_trust(h, summary.host_known.first().copied().unwrap_or(false)),
        None => url::HostTrust::Unknown,
    }
}

/// Snapshot the queue's pending entries into cards, oldest first.
///
/// ULIDs are time-sortable, so a plain sort by id is chronological.
/// Oldest-first is deliberate: the request that has been blocking an
/// agent longest is the one to answer first, and it sits at the top
/// where it is reachable without scrolling.
pub fn snapshot(pending: &[(PromptSummary, String)]) -> Vec<CardView> {
    let mut cards: Vec<CardView> = pending
        .iter()
        .map(|(summary, rendered)| card_view(summary, rendered))
        .collect();
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    cards
}

#[cfg(test)]
#[path = "../tests/cards_card.rs"]
mod tests;
