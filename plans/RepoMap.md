# Repo Map — vetter

A quick-reference structural guide for AI agents and new contributors.
For the full architecture and design rationale, read `plans/Overview.md`.

---

## 1. Workspace crates

| Crate | Role |
|-------|------|
| `vetter-core/` | Shared library: parsers (`ParsedCommand`, `Effect`), generic renderer, **`matcher`** (YAML + rules + `decide`), **`signals`** (`analyze`), **`wire`** (IPC types), paths/socket helpers. |
| `vet/` | CLI binary: `vet doctor`, `vet allow …`, `vet daemon …`, and command wrapping (`vet curl …`). Uses `vetter-core` for parse, render, matcher, signals. |
| `vetterd/` | Long-running per-user daemon: socket IPC, pending queue, notifier/UI, **`policy::evaluate`** which calls `vetter_core::matcher::decide`. |

---

## 2. Allowlist / matcher

**Spec:** `plans/Overview.md §5` — layered storage (built-in → user → project → session → denylist), rules keyed by **effects**, optional `command:` to narrow to one parser.

### Module layout (`vetter-core/src/matcher/`)

| Submodule | File | Purpose |
|-----------|------|---------|
| `rule` | `rule.rs` | `Rule`, `RuleWhen`, `HttpClause`, `UrlClause`, `HostPattern`, `FileWriteClause`, `FileReadClause` + serde |
| `loader` | `loader.rs` | YAML → `AllowlistFile`; `load_default`, `discover_project_root`, `user_allowlist_path`, `add_rule` / `remove_rule` / `write_file` |
| `decide` | `decide.rs` | `decide()`, `matches_rule()`, `Decision`, `Scope` — denylist first, then session → project → user → built-in |
| `glob` | `glob.rs` | `matches_host`, `matches_path` (segment globs `*` / `**`) |
| `url` | `url.rs` | `normalise()` URL path for matching (`.` / `..` collapse) |

### How matching works

`matches_rule` requires every **non-empty** clause in `when` to match **some** effect.
For `http`, at least one `Effect::HttpRequest` must satisfy the clause.
Optional `rule.command` must equal `parsed.command`.

### Config file paths (implementation)

- **User:** `~/.vet/allowlist.yaml` — `user_allowlist_path()` in `loader.rs`.
  _(Note: `TODO.md` Phase 2 still says `~/.config/vet/allowlist.yaml`; the code uses `~/.vet/allowlist.yaml`.)_
- **Project:** walk up from `cwd` to `.git` boundary looking for `.vet/allowlist.yaml` — `discover_project_root` in `loader.rs`.
- **Override:** `vet --allowlist PATH` loads that file (rules → project slot, deny → denylist).

### CLI / daemon wiring

- `vet/src/explain.rs` — calls `matcher::load_default`, then `matcher::decide`, after `parsed.signals.extend(analyze(&parsed))`.
- `vetterd/src/policy.rs` — `evaluate()` calls `decide(parsed, store)` and maps to `PolicyOutcome`.

---

## 3. Risk signals

**Types:** `vetter-core/src/signals/mod.rs`

- **`RiskSignal`**: `kind: SignalKind`, `detail: String`, optional `effect_idx`.
- **`SignalKind`** generic: `WriteMethod`, `AuthHeader`, `InsecureTls`, `NonStandardPort`, `IdnHost`, `RawIpLiteral`, `FileOutsideCwd`, `FileReadOutsideCwd`, `PipeToShell`.
- **`SignalKind`** parser-specific: `InsecureFlag`, `ResolveOverride`, `CacertOverride`, `UnixSocket` (produced by the curl parser, **not** by `analyze`).

**`analyze(parsed)`** — generic, walks `parsed.effects`, command-agnostic.  
Curl-only signals are built in `parsers/curl/state.rs → build_signals` and pushed into `ParsedCommand.signals` by the parser itself.

Both sets are merged by `extend` in the explain path (`vet/src/explain.rs`).

---

## 4. Curl parser

**Entry:** `vetter-core/src/parsers/curl/mod.rs` → `state::parse_argv`

| File | Purpose |
|------|---------|
| `state.rs` | URL parsing (first positional via `url::Url::parse`), fragment stripping, `HttpRequest` construction, `build_signals` |
| `flags.rs` | Tokenizer / flag table |
| `mod.rs` | Parser entry point |

**URL → policy path:** `state.rs` stores `url: url.clone()` on `HttpRequest`; the matcher reads `req.url` via `normalise(&req.url)` then `host_str()`, port, scheme, path glob in `decide.rs → matches_url`.

**Shared HTTP model:** `vetter-core/src/parsers/types.rs` — `HttpRequest { url: Url, … }` (`url` crate).

---

## 5. Allowlist YAML schema

**`AllowlistFile`** (top-level): `rules:` (allow list), `deny:` (denylist).

**`Rule`**: `id`, optional `command`, `when` (`RuleWhen`), optional `note`, `created_by`, `created_at`.

**`RuleWhen`**: optional `http`, `file_write`, `file_read` — at least one required for a sensible rule (empty `when` matches nothing per `decide.rs`).

**`HttpClause`**:
- `method` (optional)
- `url` → `UrlClause`: `scheme`, `host` (string or list / `HostPattern`), `port` list, `path` glob
- `headers_allow`: default-deny; `[]` or absent → no headers allowed; `["*"]` → allow all
- `no_body`, `query` (opt-in non-empty query string)

---

## 6. Key file paths at a glance

| Concern | Path |
|---------|------|
| Core library exports | `vetter-core/src/lib.rs` |
| Matcher entry point | `vetter-core/src/matcher/mod.rs` |
| Rule schema | `vetter-core/src/matcher/rule.rs` |
| YAML loader / config paths | `vetter-core/src/matcher/loader.rs` |
| `decide` / matching logic | `vetter-core/src/matcher/decide.rs` |
| URL normalisation | `vetter-core/src/matcher/url.rs` |
| Host/path glob matching | `vetter-core/src/matcher/glob.rs` |
| Signals types + `analyze` | `vetter-core/src/signals/mod.rs` |
| Shared HTTP / parsed types | `vetter-core/src/parsers/types.rs` |
| Curl parser | `vetter-core/src/parsers/curl/` |
| Known-hosts loader + atomic write API | `vetter-core/src/known_hosts.rs` |
| Suggestion engine (allowlist + known-host tiers) | `vetter-core/src/suggest/mod.rs` |
| Wire types (incl. `MgmtRequest::{SuggestionsFor,AddRule,AddKnownHost}`) | `vetter-core/src/wire/mod.rs` |
| CLI explain + matcher wiring | `vet/src/explain.rs` |
| Daemon policy evaluation | `vetterd/src/policy.rs` |
| Daemon Phase-5 admin handlers | `vetterd/src/suggestions.rs` |
| macOS notifier (UNUserNotificationCenter) | `vetterd/src/notifier/mac.rs` |
| macOS runloop (AppKit, status item, popover) | `vetterd/src/runloop/mac/` |
| macOS picker sheets (`Allowlist…` / `Trust host…`) | `vetterd/src/runloop/mac/popover_picker.rs` |
| Linux notifier (D-Bus / zbus) | `vetterd/src/notifier/linux.rs` (Phase 6) |
| Linux runloop (GTK4 popover, ksni tray) | `vetterd/src/runloop/linux/` (Phase 6) |
| Linux picker sheets (`Allowlist…` / `Trust host…`) | `vetterd/src/runloop/linux/popover_picker.rs` (Phase 6) |
| systemd user unit + autostart .desktop + tray icon | `vetterd/resources/{vetter.service,vetter.desktop,icons/}` (Phase 6) |
| Architecture spec | `plans/Overview.md` |
| Threat model | `plans/ThreatModel.md` |
| Testing plan | `plans/TestingPlan.md` |
| macOS operational guide | `plans/MacOSApp.md` |
| Ubuntu operational guide | `plans/UbuntuApp.md` |
| Release / signing / packaging | `plans/Release.md` |
| Current tasks | `TODO.md` |

---

## 7. Test layout

- **Unit tests:** `<crate>/src/tests/<flattened_module_name>.rs`, referenced from source via `#[path = "tests/…"] mod tests;`. E.g. `matcher/loader.rs` → `src/tests/matcher_loader.rs`.
- **Integration tests:** `<crate>/tests/` (black-box, Cargo integration test harness).
- **Snapshot tests:** `insta`. Update with `INSTA_UPDATE=always cargo test --workspace --all-features` and commit `.snap` files.
- **Corpus fixtures:** `vetter-core/tests/corpus/<parser>/` — raw argv → expected render output.
