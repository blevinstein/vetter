//! Pure lowering from queue state to what the window paints.
//!
//! Deliberately free of `gtk4`: everything here is reachable from a
//! test with no display, no session bus, and no main loop, which is
//! the only automated coverage the window can have (CI has no
//! display, `plans/LinuxApp.md` §6g). The widget assembly in
//! [`super::window`] is a thin translation of these values into
//! `gtk4` objects and owns no decisions of its own.
//!
//! Same split the macOS side arrived at, and the reason
//! `crate::cards` exists: URL segmentation, signal tones and host
//! trust are already lowered and unit-tested there, so nothing in
//! this module re-derives them.
//!
//! ## Colour
//!
//! Nothing here names a colour. `crate::cards` lowers colour
//! *choices* to semantic tones, and the one place a concrete value is
//! unavoidable — Pango markup, which takes a hex string — takes it as
//! a [`MarkupPalette`] the caller supplies. That keeps theme
//! selection (light vs dark, which only the GTK thread can answer) on
//! the widget side while the markup *structure* stays testable here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use vetter_core::matcher::{Rule, Scope};
use vetter_core::suggest::{HostSuggestion, RuleSuggestion};
use vetter_core::wire::{WireDecision, WireScope};
use vetter_core::{Auth, Body, Effect, ParsedCommand, RiskSignal, SignalKind};

use crate::cards::{
    self, markup::escape_markup, pills::PillSpec, rules::DurationChoice, spans::AnsiColor,
};
use crate::pending::{PendingDecision, PromptSummary, ResolvedEntry};

/// Audit reasons written when a decision comes from the window.
///
/// Same shape as Phase 6a's `… via admin socket` and 6b's
/// `… via notification`, and the strings `plans/LinuxApp.md` §7
/// step 13 expects to find interleaved in the log.
pub(crate) const REASON_APPROVED: &str = "approved via window";
pub(crate) const REASON_REJECTED: &str = "rejected via window";

/// The two things a card's buttons can do. Named rather than passing
/// a bare `bool` so the call site reads as a decision instead of a
/// flag, and so step 3's picker actions have somewhere to land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardAction {
    Approve,
    Reject,
}

impl CardAction {
    /// Lower a button press into the decision the queue records.
    pub(crate) fn decision(self) -> PendingDecision {
        match self {
            CardAction::Approve => PendingDecision::allow(REASON_APPROVED),
            CardAction::Reject => PendingDecision::deny(REASON_REJECTED),
        }
    }
}

// ── Disclosure state ────────────────────────────────────────────────────────

/// Which collapsible section of a card a disclosure drives.
///
/// Three kinds rather than three `HashSet` fields threaded through
/// three near-identical accessor pairs: the widget side just wants
/// "is *this* disclosure on *this* card open", and naming the kinds
/// keeps that a single call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Disclosure {
    /// "Show raw" — the §8.5 body. On every card.
    Raw,
    /// "Details" — the structured effect rows. Resolved cards only:
    /// a pending card shows its rows inline, because someone
    /// deciding *now* should not have to go looking for what the
    /// command does.
    Details,
    /// "See approval reason" — rule attribution plus the revoke
    /// button. Auto-allowed resolved cards only.
    Reason,
}

/// Which collapsible sections of which cards are currently open.
///
/// This lives in the model rather than in the `Expander` widgets for
/// a reason worth spelling out: [`super::window::refresh`] rebuilds
/// every card from scratch, and it runs on *every* queue change —
/// including changes that have nothing to do with the card the user
/// is reading. A notification approved on another card, a
/// `vet daemon approve` over the admin socket, a rule addition that
/// auto-resolves something: any of those would collapse an open
/// disclosure mid-read if the open/closed bit lived in the widget
/// that gets destroyed.
///
/// Keyed by `(kind, request ULID)`, so the state follows the card
/// rather than its position in the list, and a card's three
/// disclosures stay independent of one another.
#[derive(Debug, Default)]
pub(crate) struct ExpandedState {
    /// Absent means closed, which is the default the spec asks for
    /// ("Default state: collapsed").
    open: HashSet<(Disclosure, String)>,
}

impl ExpandedState {
    pub(crate) fn is_open(&self, kind: Disclosure, id: &str) -> bool {
        // `HashSet::contains` over an owned key pair: the tuple would
        // need a matching `Borrow` impl to probe by reference, which
        // is not worth a newtype for a set this small (bounded by the
        // number of cards on screen).
        self.open.contains(&(kind, id.to_string()))
    }

    pub(crate) fn set_open(&mut self, kind: Disclosure, id: &str, open: bool) {
        let key = (kind, id.to_string());
        if open {
            self.open.insert(key);
        } else {
            self.open.remove(&key);
        }
    }

    /// Forget ids the window is no longer showing.
    ///
    /// Without this the set grows for the lifetime of the daemon:
    /// every request whose disclosure was ever opened would leave a
    /// ULID behind after it fell out of view. The live set spans
    /// *both* sections — a resolved card is still on screen, so
    /// pruning against pending ids alone would collapse the Recent
    /// disclosure the user just opened.
    pub(crate) fn retain_live(&mut self, live_ids: &[&str]) {
        self.open
            .retain(|(_, id)| live_ids.iter().any(|live| live == id));
    }
}

// ── URL row ─────────────────────────────────────────────────────────────────

/// The URL row's typed tokens, or the fallback line for cards with
/// no HTTP effect.
///
/// See `plans/ApprovalUI.md` "URL row" for the token-by-token style
/// table; every decision about *which* tokens appear is made in
/// [`crate::cards::url`], not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlView {
    Http(HttpUrlView),
    /// `<command>  <verb> <target>`, for `ProcessSpawn`-only cards and
    /// for summaries that predate the `parsed` field. Keeps the slot
    /// populated so the card's vertical cadence is constant.
    Fallback(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpUrlView {
    pub method: String,
    pub method_tone: cards::url::MethodTone,
    /// `https://`, including the separator.
    pub scheme: String,
    pub host: String,
    pub trust: cards::url::HostTrust,
    /// Only present when the port is worth showing at all — see
    /// [`crate::cards::url::visible_port`].
    pub port: Option<u16>,
    pub tail: cards::url::PathQuery,
}

// ── Effect rows ─────────────────────────────────────────────────────────────

/// A file path plus whether the window should offer to open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilePath {
    pub display: String,
    pub path: PathBuf,
    /// False when the path does not exist, which suppresses the
    /// `Open file` button. Same rule as macOS: offering to open a
    /// file that is not there produces a confusing no-op, and for a
    /// `FileWrite` the file usually does not exist *yet*.
    pub can_open: bool,
}

/// Body cell content, already lowered to strings by
/// [`crate::cards::effects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyContent {
    Inline(String),
    FromFile(FilePath),
    Form(Vec<String>),
}

/// One row beneath the URL. Mirrors the per-`Effect` catalogue in
/// `plans/ApprovalUI.md` "Effect rows".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EffectRow {
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
        /// [`crate::cards::effects::auth_label`].
        redacted: bool,
    },
    FileRead(FilePath),
    FileWrite(FilePath),
    ProcessSpawn(String),
}

// ── Card ────────────────────────────────────────────────────────────────────

/// One pending request, lowered to the strings and tones the window
/// paints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardView {
    /// Correlation ULID. The identity the buttons resolve against,
    /// and the key [`ExpandedState`] tracks disclosures by.
    pub id: String,
    /// Title line, e.g. `curl`.
    pub title: String,
    /// True when `vet --dry-run` forced this onto the prompt path.
    /// Drives the dry-run wrapper rather than an inline pill, so the
    /// two classes of card read apart at a glance when both are in
    /// the list (`plans/ApprovalUI.md` "Dry-run wrapper").
    pub dry_run: bool,
    pub url: UrlView,
    /// Signal pills, deduped by kind and sorted most-urgent-first.
    pub pills: Vec<PillSpec>,
    /// Per-effect rows, file inputs first (see [`effect_rows`]).
    pub rows: Vec<EffectRow>,
    /// The §8.5 raw body behind the "Show raw" disclosure.
    pub raw: RawView,
    /// Host-trust for the primary host, also carried on
    /// [`HttpUrlView`]. Kept at card level so a fallback card can
    /// still colour its chrome.
    pub trust: cards::url::HostTrust,
    /// Whether the `Trust host…` button belongs on this card. See
    /// [`show_trust_host`].
    pub show_trust_host: bool,
}

/// The "Show raw" disclosure's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawView {
    /// Styled runs, for painting.
    pub spans: Vec<cards::spans::AnsiSpan>,
    /// The same text with SGR escapes stripped, for the clipboard.
    /// Kept separate so the copy button writes exactly the bytes the
    /// user can see rather than anything the markup layer added.
    pub plain: String,
}

/// What the window shows when it has nothing to approve. Kept here
/// rather than inline in the widget code so the empty state is
/// covered by a test like every other state.
pub(crate) const EMPTY_TITLE: &str = "No pending approvals";
pub(crate) const EMPTY_BODY: &str =
    "Requests that need a decision will appear here. You can also approve \
     from the tray menu, a notification, or `vet daemon approve`.";

/// Lower one summary plus its pre-rendered §8.5 detail into a card.
///
/// Every argv-derived string goes through
/// [`vetter_core::render::sanitize_for_display`] — mostly inside
/// `crate::cards`, which sanitises at the point each string is built.
/// A hostile URL carrying RTLO or zero-width bytes would otherwise be
/// painted verbatim onto the surface a human uses to authorise it.
/// Escaping for markup is a *separate* concern applied at the point
/// text enters Pango; see [`spans_to_markup`].
pub(crate) fn card_view(summary: &PromptSummary, rendered: &str) -> CardView {
    use vetter_core::render::sanitize_for_display;

    let title = sanitize_for_display(&summary.command).into_owned();
    let trust = trust_of(summary);

    CardView {
        id: summary.id.clone(),
        title,
        dry_run: summary.force_prompt,
        url: url_view(summary, trust),
        pills: pills_for(&summary.signals),
        rows: summary.parsed.as_ref().map(effect_rows).unwrap_or_default(),
        raw: RawView {
            spans: cards::spans::parse_ansi_spans(rendered),
            plain: cards::spans::strip_ansi(rendered),
        },
        trust,
        show_trust_host: show_trust_host(&summary.signals),
    }
}

/// The URL row for this summary, or the fallback line.
fn url_view(summary: &PromptSummary, trust: cards::url::HostTrust) -> UrlView {
    use vetter_core::render::sanitize_for_display;

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
            method_tone: cards::url::method_tone(&req.method),
            scheme: format!("{}://", req.url.scheme()),
            host: sanitize_for_display(req.url.host_str().unwrap_or_default()).into_owned(),
            trust,
            port: cards::url::visible_port(&req.url),
            tail: cards::url::path_query(&req.url),
        }),
        None => UrlView::Fallback(cards::url::fallback_text(
            &summary.command,
            &summary.primary_verb,
            &summary.primary_target,
        )),
    }
}

/// Host-trust for the summary's primary host.
///
/// `host_known` is computed daemon-side against the known-hosts store
/// (the window cannot reach it), so entry 0 — the primary HTTP
/// effect — is what the pill reflects. Falls back to "unknown" when
/// the summary predates that field or carries no HTTP effect, which
/// is the conservative direction: an unknown-host pill overstates
/// risk, a known-host pill would understate it.
fn trust_of(summary: &PromptSummary) -> cards::url::HostTrust {
    let host = summary
        .parsed
        .as_ref()
        .and_then(|p| p.effects.first())
        .and_then(|e| match e {
            Effect::HttpRequest(req) => req.url.host_str(),
            _ => None,
        });
    match host {
        Some(h) => cards::url::host_trust(h, summary.host_known.first().copied().unwrap_or(false)),
        None => cards::url::HostTrust::Unknown,
    }
}

/// Signal pills for a card: one chip per [`SignalKind`], most urgent
/// first.
///
/// Dedupe is by kind, keeping the *first* signal's detail for the
/// tooltip — a multi-effect request that trips the same kind three
/// times gets one chip, not three, so the pills row cannot bury the
/// rest of the card. `Info`-tier kinds yield no chip at all and live
/// only in the raw body's `Risk signals:` line.
///
/// The sort is stable, so within a tone the analyzer's emission order
/// survives; only the tone tiers move.
pub(crate) fn pills_for(signals: &[RiskSignal]) -> Vec<PillSpec> {
    let mut seen: HashSet<SignalKind> = HashSet::new();
    let mut out: Vec<(u8, PillSpec)> = Vec::new();
    for sig in signals {
        if !seen.insert(sig.kind) {
            continue;
        }
        if let Some(spec) = cards::pills::signal_pill(sig.kind, &sig.detail) {
            out.push((cards::pills::signal_priority(sig.kind), spec));
        }
    }
    out.sort_by_key(|(priority, _)| *priority);
    out.into_iter().map(|(_, spec)| spec).collect()
}

/// Lower `parsed.effects` into rows.
///
/// Ordering mirrors the macOS popover: file-input rows first (the
/// "what file are you uploading?" question belongs at the top of the
/// card), then everything else in source order so a single card's
/// headers / body / auth sequence matches the §8.5 renderer.
///
/// `FileRead`s whose path is already shown by a `Body::FromFile` row
/// are dropped: the curl parser deliberately emits both for
/// `-d @file` so the matcher can reason about the read, but a card
/// rendering both would show the same path twice.
/// `CredentialUse` and `Network` are skipped, as they are in the
/// §8.5 layout.
pub(crate) fn effect_rows(parsed: &ParsedCommand) -> Vec<EffectRow> {
    let body_files = cards::effects::collect_body_file_paths(parsed);
    let mut file_inputs: Vec<EffectRow> = Vec::new();
    let mut others: Vec<EffectRow> = Vec::new();

    for eff in &parsed.effects {
        match eff {
            Effect::HttpRequest(req) => {
                if !req.headers.is_empty() {
                    others.push(EffectRow::Headers(
                        req.headers
                            .iter()
                            .map(|h| {
                                vetter_core::render::sanitize_for_display(&h.name).into_owned()
                            })
                            .collect(),
                    ));
                }
                if let Some(meta) = cards::effects::body_meta_label(&req.body) {
                    let content = match &req.body {
                        Body::Inline { bytes } => {
                            Some(BodyContent::Inline(cards::effects::inline_body_text(bytes)))
                        }
                        Body::FromFile { path } => Some(BodyContent::FromFile(file_path(path))),
                        Body::Form { fields } => Some(BodyContent::Form(
                            fields.iter().map(cards::effects::form_field_text).collect(),
                        )),
                        Body::None => None,
                    };
                    if let Some(content) = content {
                        let row = EffectRow::Body { meta, content };
                        // A file body is a file-input row; an inline
                        // or form body is not.
                        match &row {
                            EffectRow::Body {
                                content: BodyContent::FromFile(_),
                                ..
                            } => file_inputs.push(row),
                            _ => others.push(row),
                        }
                    }
                }
                if let Some(auth) = req.auth.as_ref() {
                    let (text, redacted) = auth_row(auth);
                    others.push(EffectRow::Auth { text, redacted });
                }
            }
            Effect::FileRead(read) => {
                if !body_files.contains(&read.path) {
                    file_inputs.push(EffectRow::FileRead(file_path(&read.path)));
                }
            }
            Effect::FileWrite(write) => {
                others.push(EffectRow::FileWrite(file_path(&write.path)));
            }
            Effect::ProcessSpawn(spawn) => {
                others.push(EffectRow::ProcessSpawn(
                    vetter_core::render::sanitize_for_display(&spawn.command).into_owned(),
                ));
            }
            Effect::CredentialUse(_) | Effect::Network(_) => {}
        }
    }

    file_inputs.extend(others);
    file_inputs
}

fn auth_row(auth: &Auth) -> (String, bool) {
    cards::effects::auth_label(auth)
}

/// Lower a path into its display string plus the `Open file`
/// suppression answer.
pub(crate) fn file_path(path: &Path) -> FilePath {
    FilePath {
        display: vetter_core::render::sanitize_for_display(&path.display().to_string())
            .into_owned(),
        path: path.to_path_buf(),
        can_open: can_open(path),
    }
}

/// Whether the window should offer an `Open file` button for `path`.
///
/// Existence is the whole rule, and it is checked at card-build time
/// rather than at click time so the button is absent rather than
/// present-and-broken. A `FileWrite` target usually does not exist
/// yet, which is exactly the case this suppresses.
pub(crate) fn can_open(path: &Path) -> bool {
    path.exists()
}

// ── Markup ──────────────────────────────────────────────────────────────────

/// Concrete colours for [`spans_to_markup`].
///
/// Pango markup takes hex strings, so this is the one place a colour
/// value has to appear. It is a parameter rather than a constant
/// because only the GTK thread can ask which theme is active, and the
/// palette that reads well on a dark surface is illegible on a light
/// one. Keeping it injected also means the markup *structure* can be
/// tested against a fixed palette without a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkupPalette {
    pub red: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub magenta: &'static str,
    pub cyan: &'static str,
    pub blue: &'static str,
    /// Used for bright-black and for the dimmed-cyan loopback run.
    pub dim: &'static str,
}

/// Render parsed ANSI spans as Pango markup.
///
/// **Every span's text is escaped** ([`escape_markup`]) before it is
/// wrapped in a tag. The text comes from argv; without escaping, a
/// URL containing `<span foreground='…'>` would be parsed as markup
/// on the surface a human reads to decide whether to approve the
/// command — it could recolour or hide the very text describing what
/// is about to run. This is the same hole the notification
/// `body-markup` path closes, which is why both call the same escape.
///
/// Spans carrying no style emit bare escaped text rather than an
/// empty `<span>`, which keeps the markup readable in a test failure.
pub(crate) fn spans_to_markup(spans: &[cards::spans::AnsiSpan], palette: MarkupPalette) -> String {
    let mut out = String::new();
    for span in spans {
        let text = escape_markup(&span.text);
        let mut attrs = String::new();
        if let Some(colour) = span_colour(span.style, palette) {
            attrs.push_str(&format!(" foreground=\"{colour}\""));
        }
        if span.style.bold {
            attrs.push_str(" weight=\"bold\"");
        }
        if span.style.underline {
            attrs.push_str(" underline=\"single\"");
        }
        if attrs.is_empty() {
            out.push_str(&text);
        } else {
            out.push_str(&format!("<span{attrs}>{text}</span>"));
        }
    }
    out
}

/// Resolve one span's colour against the palette.
///
/// Two cases are not a straight table lookup, and both come from
/// `plans/ApprovalUI.md` "Body colouring":
/// - cyan + dim is the loopback style (`2;36`), which reads as muted
///   rather than as the teal used for a live URL;
/// - bright-black is the renderer's "secondary" colour, so it maps to
///   the same dim value.
fn span_colour(style: cards::spans::SpanStyle, palette: MarkupPalette) -> Option<&'static str> {
    match style.color? {
        AnsiColor::Cyan if style.dim => Some(palette.dim),
        AnsiColor::Cyan => Some(palette.cyan),
        AnsiColor::Red => Some(palette.red),
        AnsiColor::Green => Some(palette.green),
        AnsiColor::Yellow => Some(palette.yellow),
        AnsiColor::Magenta => Some(palette.magenta),
        AnsiColor::BrightBlack => Some(palette.dim),
        AnsiColor::BrightBlue => Some(palette.blue),
    }
}

// ── Resolved cards ──────────────────────────────────────────────────────────

/// How a resolved request came out. Narrower than
/// [`vetter_core::wire::WireDecision`] on purpose: the window paints
/// two outcomes, and `AllowOnce` — which no daemon emits today —
/// reads as an allow rather than as a third badge nobody has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Allowed,
    Denied,
}

impl Outcome {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Outcome::Allowed => "allowed",
            Outcome::Denied => "denied",
        }
    }
}

/// Why a resolved card was auto-allowed, and whether the window can
/// take the rule back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleAttribution {
    pub rule_id: String,
    pub scope: Scope,
    /// Sentence shown when "See approval reason" is opened.
    pub summary: String,
    /// `Some(scope)` when "Revoke rule" should be offered and where
    /// the removal would land; `None` when this layer is not editable
    /// from the window — see [`revoke_scope`].
    pub revoke: Option<WireScope>,
}

/// One entry from the resolved-history ring, lowered for painting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedCardView {
    /// Same shape as a pending card, so the two sections share every
    /// row builder. Only the chrome around it differs.
    pub card: CardView,
    pub outcome: Outcome,
    /// `Some` only for auto-allowed entries the matcher attributed to
    /// a rule. Drives "See approval reason" / "Revoke rule".
    pub attribution: Option<RuleAttribution>,
    /// Whether the picker row appears. Allow-resolved cards keep it
    /// (the user may still want a standing rule); deny-resolved cards
    /// drop it — offering "Allowlist…" immediately after someone
    /// pressed Reject reads as arguing with them.
    pub show_pickers: bool,
}

/// Which allowlist layer a revoke would write to, or `None` when the
/// window cannot edit it.
///
/// Session-scoped rules live in the *same* on-disk user file as any
/// other rule (the loader partitions them at load time), so
/// `Scope::Session` is revokable through the identical `User` remove
/// call rather than being a dead end. Built-in, project and denylist
/// entries are not editable here: project rules belong to
/// `vet allow rm`, and the other two are not user-owned at all.
pub(crate) fn revoke_scope(scope: Scope) -> Option<WireScope> {
    match scope {
        Scope::User | Scope::Session => Some(WireScope::User),
        Scope::Project | Scope::Builtin | Scope::Denylist => None,
    }
}

/// Lower one resolved-history entry into a card.
pub(crate) fn resolved_card_view(entry: &ResolvedEntry) -> ResolvedCardView {
    let outcome = match entry.decision {
        WireDecision::Allow | WireDecision::AllowOnce => Outcome::Allowed,
        WireDecision::Deny => Outcome::Denied,
    };
    // Attribution needs both halves. `rule_scope` is documented as
    // always populated alongside `rule_id`, but zipping rather than
    // unwrapping means a future producer that sets only one cannot
    // panic the UI thread.
    let attribution = entry
        .rule_id
        .as_ref()
        .zip(entry.rule_scope)
        .filter(|_| outcome == Outcome::Allowed)
        .map(|(rule_id, scope)| RuleAttribution {
            rule_id: rule_id.clone(),
            scope,
            summary: format!(
                "Auto-allowed by rule `{rule_id}` in {} scope.",
                scope.as_str()
            ),
            revoke: revoke_scope(scope),
        });

    ResolvedCardView {
        card: card_view(&entry.summary, &entry.rendered),
        outcome,
        attribution,
        show_pickers: outcome == Outcome::Allowed,
    }
}

// ── Picker view-models ──────────────────────────────────────────────────────

/// One radio row in a picker: the tier headline and the YAML the
/// user would be persisting.
///
/// `preview` is deliberately a plain string destined for `set_text`.
/// It is built from argv-derived data, and the one thing it must
/// never become is markup — see the module docs in [`super::window`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PickerRow {
    pub title: String,
    pub preview: String,
}

/// Rows for the "Allowlist…" picker, tightest tier first (the order
/// [`vetter_core::suggest::allowlist_suggestions`] emits).
pub(crate) fn rule_picker_rows(suggestions: &[RuleSuggestion]) -> Vec<PickerRow> {
    suggestions
        .iter()
        .map(|s| PickerRow {
            title: format!("{}  —  {}", s.tier.as_str(), s.label),
            preview: cards::rules::render_rule_when_yaml(&s.rule),
        })
        .collect()
}

/// Rows for the "Trust host…" picker.
pub(crate) fn host_picker_rows(suggestions: &[HostSuggestion]) -> Vec<PickerRow> {
    suggestions
        .iter()
        .map(|s| PickerRow {
            title: format!("{}  —  {}", s.tier.as_str(), s.label),
            preview: format!("pattern: \"{}\"", s.entry.pattern),
        })
        .collect()
}

/// Stamp a duration choice onto a suggested rule.
///
/// The whole reason [`crate::cards::rules::DurationChoice`] was
/// lifted into the shared layer: a 15-minute rule must expire in 15
/// minutes and a session rule must carry the requesting terminal's
/// sid whether the radio was an `NSButton` or a `GtkCheckButton`.
pub(crate) fn rule_with_duration(
    mut rule: Rule,
    choice: DurationChoice,
    now: u64,
    peer_sid: Option<i32>,
) -> Rule {
    let (expires_at, sid) = choice.apply(now, peer_sid);
    rule.expires_at = expires_at;
    rule.sid = sid;
    rule
}

/// Whether the "Trust host…" button should appear for these signals.
///
/// Gated on `UnknownHost` being present, mirroring the
/// `host_suggestions` engine contract: when the host is already
/// trusted there is nothing to suggest, and a button that opens an
/// empty picker is worse than no button.
pub(crate) fn show_trust_host(signals: &[RiskSignal]) -> bool {
    signals.iter().any(|s| s.kind == SignalKind::UnknownHost)
}

/// Header shown above the resolved section.
pub(crate) const RECENT_TITLE: &str = "Recent";
/// Placeholder standing in for the pending block when it is empty but
/// Recent is not. Without it the window opens straight onto resolved
/// cards with no action buttons and no explanation.
pub(crate) const NO_PENDING: &str = "No pending approvals.";

/// Snapshot the queue's pending entries into cards, oldest first.
///
/// ULIDs are time-sortable, so a plain sort by id is chronological.
/// Oldest-first is deliberate and differs from the tray's newest-last
/// ordering only in framing: the request that has been blocking an
/// agent longest is the one the user should answer first, and it sits
/// at the top where it is reachable without scrolling.
pub(crate) fn snapshot(pending: &[(PromptSummary, String)]) -> Vec<CardView> {
    let mut cards: Vec<CardView> = pending
        .iter()
        .map(|(summary, rendered)| card_view(summary, rendered))
        .collect();
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    cards
}

/// Lower the resolved-history ring for painting.
///
/// Order is left exactly as the queue holds it — newest first, since
/// [`crate::pending::PendingQueue::resolve`] pushes to the front.
/// That is the opposite of the pending section's oldest-first sort,
/// and deliberately so: pending answers "what has been waiting
/// longest", Recent answers "what just happened".
pub(crate) fn resolved_snapshot(resolved: &[ResolvedEntry]) -> Vec<ResolvedCardView> {
    resolved.iter().map(resolved_card_view).collect()
}

/// Every id currently on screen, across both sections. Feeds
/// [`ExpandedState::retain_live`].
pub(crate) fn live_ids<'a>(
    pending: &'a [CardView],
    resolved: &'a [ResolvedCardView],
) -> Vec<&'a str> {
    pending
        .iter()
        .map(|c| c.id.as_str())
        .chain(resolved.iter().map(|r| r.card.id.as_str()))
        .collect()
}

#[cfg(test)]
#[path = "../../tests/window_model.rs"]
mod tests;
