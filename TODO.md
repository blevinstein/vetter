# vetter — Project TODO

Tracks per-phase progress against the roadmap in
[plans/Overview.md](plans/Overview.md) §11. Phase exit criteria come from
[plans/TestingPlan.md](plans/TestingPlan.md) §11.

Status legend: `[x]` done · `[~]` in progress · `[ ]` not started.

---

## Phase 0 — Skeleton  `[x] done`

- [x] Cargo workspace with `vet`, `vetterd`, `vetter-core`
      (see [Cargo.toml](Cargo.toml))
- [x] `rust-toolchain.toml` pinned to `stable`
- [x] `vet doctor` stub ([vet/src/doctor.rs](vet/src/doctor.rs))
- [x] `vetterd` Phase 0 stub ([vetterd/src/main.rs](vetterd/src/main.rs))
- [x] CI: fmt, clippy (Ubuntu) + test matrix (Ubuntu + macOS)
      ([.github/workflows/ci.yml](.github/workflows/ci.yml))
- [x] Smoke tests for `vet doctor` and `vetterd`
      ([vet/tests/doctor.rs](vet/tests/doctor.rs),
      [vetterd/tests/smoke.rs](vetterd/tests/smoke.rs))

## Phase 1a — Plugin scaffolding  `[x] done`

- [x] `ParsedCommand` + `Effect` (HttpRequest / FileWrite / FileRead /
      ProcessSpawn / CredentialUse / Network) and supporting types
      ([vetter-core/src/parsers/types.rs](vetter-core/src/parsers/types.rs))
- [x] `CommandParser` trait, `ParseError`, `StdinHandle`, `EnvSnapshot`
      ([vetter-core/src/parsers/mod.rs](vetter-core/src/parsers/mod.rs))
- [x] Static parser registry with `register` / `dispatch` / `all` and
      basename matching
- [x] Generic renderer (PlainWriter + AnsiWriter) producing the §8.5
      layout, with header redaction
      ([vetter-core/src/render/](vetter-core/src/render/))
- [x] Generic risk analyzer covering all §9 generic signal kinds
      ([vetter-core/src/signals/mod.rs](vetter-core/src/signals/mod.rs))
- [x] Test-only `noop` parser
      ([vetter-core/src/parsers/noop.rs](vetter-core/src/parsers/noop.rs))
- [x] §2.1 type roundtrips, §2.2 registry dispatch, §2.3 noop snapshot
      (insta), §2.4 per-SignalKind tests

## Phase 1b — Curl parser  `[x] done`

Roadmap: [plans/Overview.md](plans/Overview.md) §8.4, §11. Test plan:
[plans/TestingPlan.md](plans/TestingPlan.md) §3.

- [x] `parsers/curl/` module with full argv parser covering the curl
      flag set (`flags.rs`)
- [x] Emits `Effect::HttpRequest` (+ optional `FileWrite` for
      `-o`/`-O`/`-J`, `FileRead` for `-T <file>` and `-d @file`)
- [x] Curl-specific `RiskSignal`s pushed during parse: `--insecure` /
      `-k`, `--cacert`, `--resolve`, `--unix-socket`
- [x] Refuses streaming bodies with `ParseError::StreamingUnsupported`
      (`-T -`, chunked transfer, `-d @-` >1 MiB)
- [x] Snapshot fixture corpus in `vetter-core/tests/corpus/curl/` per
      `TestingPlan.md` §3.1 (Phase 1b ships ~12 fixtures; expand toward
      the full ~25 in a follow-up phase)
- [x] `vet --explain curl …` runs end-to-end against the Phase 1a
      renderer + analyzer with no daemon, no policy
- [x] CLI: TTY / `NO_COLOR` / `CLICOLOR_FORCE` detection chooses
      `PlainWriter` vs `AnsiWriter`
- [x] `vet doctor` reports `parsers registered . 1` (curl)

## Phase 2 — Allowlist evaluation  `[x] done`

Roadmap: [plans/Overview.md](plans/Overview.md) §5. Test plan:
[plans/TestingPlan.md](plans/TestingPlan.md) §2.5, §2.6.

- [x] YAML schema + loader for user scope (`~/.config/vet/allowlist.yaml`)
      and project scope (`<repo>/.vet/allowlist.yaml`); walks up from cwd
- [x] Rule matcher in `vetter-core::matcher` against `effects`; layered
      precedence (denylist > session > project > user > built-in)
- [x] `headers_allow` default-deny semantics; explicit `*` opt-in
- [x] Path/host glob matching; URL normalisation before match
- [x] `vet --explain curl …` reports allow / deny / prompt with
      matched rule id
- [x] `vet allow add` / `rm` / `list` subcommands fully wired
- [x] Property tests over rule matching (TestingPlan §2.5)
- [x] Negative tests: host-suffix confusion, path traversal, denylist
      overrides allow at every layer

## Phase 3 — Daemon + IPC  `[x] done`

Roadmap: [plans/Overview.md](plans/Overview.md) §3, §11. Test plan:
[plans/TestingPlan.md](plans/TestingPlan.md) §5, §6.

Phase 3a (spine) landed; remaining boxes are Phase 3b polish.

- [x] `vetterd` socket listener at `$TMPDIR/vetter.sock`; one
      request → one decision; length-prefixed JSON
- [x] `VetRequest` / `VetDecision` wire types in `vetter-core::wire`
      with `v` field for version negotiation
- [x] Pending-request queue; concurrent clients get independent
      decisions
- [x] `vet` becomes a thin client; fails closed (exit non-zero) when
      the socket is missing — no TTY fallback
- [x] Audit log at `~/Library/Logs/vetter/audit.log` (JSON lines),
      flushed before decision returns
- [x] Stub UI for prompt-class decisions: auto-deny with reason
      `"no UI yet"`
- [x] `vet daemon start|stop|status` controls supervise `vetterd`
- [x] Atomic allowlist writes (temp + rename); §5.3 crash test
- [x] §4.9 fail-closed test (no exec on deny / missing socket)
- [x] §6.3 protocol fuzzing entry

## Phase 4 — macOS approver UI  `[~] in progress`  (MVP milestone)

Roadmap: [plans/Overview.md](plans/Overview.md) §7. Operational notes
+ manual smoke procedure: [plans/MacOSApp.md](plans/MacOSApp.md).

PR 1 lands the approve/reject happy path: an all-in-Rust
`Vetter.app` bundle, `UNUserNotificationCenter` notifications with
Approve / Reject buttons, and the full pending-queue → notifier round
trip. PR 2 adds the menu-bar shield icon, a pending-count badge, an
`NSPopover` listing every pending request with §8.5 detail and
per-card Approve / Reject, and click-through routing from the
notification body into that popover. Banner coalescing and the
local Developer-ID signing + notarisation pipeline have since
landed. Remaining open items below are all deferred to the Backlog
(CI release workflow, signed-bundle E2E, banner-side
`Allowlist…` action) — they are convenience, not v0.1
ship-blockers.

- [x] All-in-Rust menu-bar `.app` bundle, `LSUIElement`, no dock icon
      (`tools/build-app.sh`,
      [vetterd/resources/Info.plist.template](vetterd/resources/Info.plist.template),
      [vetterd/src/runloop/mod.rs](vetterd/src/runloop/mod.rs))
- [x] `UNUserNotificationCenter` integration: Approve / Reject
      actions wired through
      [vetterd/src/notifier/mac.rs](vetterd/src/notifier/mac.rs) +
      [vetterd/src/runloop/mod.rs](vetterd/src/runloop/mod.rs)
- [x] Pending-queue spine + `Notifier` trait + `MockNotifier` for
      test-driven coverage
      ([vetterd/src/pending.rs](vetterd/src/pending.rs),
      [vetterd/src/notifier/](vetterd/src/notifier/))
- [x] `policy::evaluate` returns `PolicyOutcome::{Auto, Prompt}`
      (Phase 3a stub-deny removed)
      ([vetterd/src/policy.rs](vetterd/src/policy.rs))
- [x] Audit log records the human-driven decision, not a stub
- [x] Mock-driven E2E: approve, reject, concurrent prompts,
      shutdown-while-pending
      ([vetterd/tests/daemon_e2e_prompt.rs](vetterd/tests/daemon_e2e_prompt.rs))
- [x] Ad-hoc code-signed `.app` for local dev (`tools/build-app.sh`)
- [x] Manual smoke procedure documented
      ([plans/MacOSApp.md](plans/MacOSApp.md))
- [x] Menu-bar shield icon with pending-count badge updated by the
      `PendingQueue` change listener
      ([vetterd/src/runloop/status_item.rs](vetterd/src/runloop/status_item.rs))
- [x] Popover listing pending requests with §8.5 rendered summary
      and detail view, plus per-card Approve / Reject
      ([vetterd/src/runloop/popover.rs](vetterd/src/runloop/popover.rs))
- [x] Notification body click-through routes to the popover
      (focused-id auto-scroll); request stays pending until the user
      Approves / Rejects in the popover
- [x] Banner removed via `removeDeliveredNotificationsWithIdentifiers:`
      on every resolve path so Notification Center stays clean
- [x] `Allowlist…` (and `Trust host…`) actions on the notification
      banner itself: notification category gates the Trust host…
      button on the request actually carrying an `UnknownHost`
      signal so already-trusted hosts don't see the extra action;
      both buttons lift the same picker `NSAlert` the popover
      uses, and `persist_rule_async` now also clears delivered
      banners for any auto-approved id so the banner doesn't
      linger after a covering rule is added
      ([vetterd/src/runloop/mod.rs](vetterd/src/runloop/mod.rs),
      [vetterd/src/notifier/mac.rs](vetterd/src/notifier/mac.rs),
      [vetterd/src/runloop/popover_picker.rs](vetterd/src/runloop/popover_picker.rs))
- [x] Notification coalescing into menu-bar after the first banner
      ([vetterd/src/notifier/mac.rs](vetterd/src/notifier/mac.rs)
      consumes `NotifyHint::was_empty_before` from
      [vetterd/src/pending.rs](vetterd/src/pending.rs))
- [x] Real Developer-ID signing + notarisation pipeline; Homebrew tap
      ([tools/release.sh](tools/release.sh),
      [tools/_bundle_layout.sh](tools/_bundle_layout.sh),
      [vetterd/resources/vetterd.entitlements](vetterd/resources/vetterd.entitlements),
      [plans/Release.md](plans/Release.md); cask lives in the
      separate [`blevinstein/homebrew-vetter`](https://github.com/blevinstein/homebrew-vetter)
      tap repo). Local pipeline only; automated CI release workflow
      tracked separately below.
- [x] `vet daemon list` — CLI command to list pending approvals over
      a new admin socket (`vetter-admin.sock`); `vet daemon status`
      now shows real pending count
      ([vet/src/daemon.rs](vet/src/daemon.rs),
      [vetterd/src/lib.rs](vetterd/src/lib.rs),
      [vetter-core/src/wire/mod.rs](vetter-core/src/wire/mod.rs),
      [vetterd/tests/admin_ipc.rs](vetterd/tests/admin_ipc.rs))

### Autostart on login

Removes the open caveat that the user has to `open
/Applications/Vetter.app` once after every reboot. Architectural
notes + reconciliation contract live in
[plans/MacOSApp.md § Autostart on login](plans/MacOSApp.md#autostart-on-login).

- [x] `vetter-core::settings` user-pref store at
      `~/.vet/settings.yaml`, atomic write at mode `0600`
      ([vetter-core/src/settings.rs](vetter-core/src/settings.rs))
- [x] `vetterd::autostart` driver: `SMAppService.mainApp` FFI,
      `current` / `enable` / `disable` / `reconcile_with_settings`,
      bundle-location guard mirroring `notifier::mac`
      ([vetterd/src/autostart.rs](vetterd/src/autostart.rs))
- [x] Wire + CLI: `MgmtRequest::{GetAutostart, SetAutostart}` over
      the admin socket; `vet daemon autostart enable | disable |
      status`
      ([vetter-core/src/wire/mod.rs](vetter-core/src/wire/mod.rs),
      [vet/src/daemon.rs](vet/src/daemon.rs))
- [x] Popover footer **Start at login** checkbox alongside Quit;
      tied to `toggleAutostart:` selector with UI rollback when
      `SMAppService.{register,unregister}` errors; refreshed from
      `[SMAppService.mainApp status]` on every `popoverWillShow:`
      ([vetterd/src/runloop/popover.rs](vetterd/src/runloop/popover.rs),
      [vetterd/src/runloop/mod.rs](vetterd/src/runloop/mod.rs))
- [x] `vet doctor` autostart row mapping `AutostartStatus` to
      OK / INFO / WARN / SKIP, falling back to a settings-only
      read when the daemon is offline
      ([vet/src/doctor.rs](vet/src/doctor.rs))
- [x] `LSMinimumSystemVersion` bumped 11.0 -> 13.0 since
      SMAppService is macOS 13+
      ([vetterd/resources/Info.plist.template](vetterd/resources/Info.plist.template))
- [x] Smoke step #12 in
      [plans/MacOSApp.md](plans/MacOSApp.md): tick the checkbox,
      log out + back in, confirm shield reappears without manually
      opening anything

## Phase 5 — Pattern + Known-Host Suggestions  `[~] in progress`

Roadmap: [plans/Overview.md](plans/Overview.md) §5 ("Pattern
suggestions"), §11. Picker-sheet UI design lives in
[plans/ApprovalUI.md](plans/ApprovalUI.md) §1.4.

- [x] Allowlist generalisation engine in
      [vetter-core/src/suggest/](vetter-core/src/suggest/) emits
      Exact → PathGlob → MethodHost tiers from a `ParsedCommand`'s
      first HttpRequest effect; lifted `derive_auto_id` from
      `vet/src/allow.rs` into `vetter-core::matcher` so daemon and
      CLI share one impl
- [x] Known-host suggestion engine in the same module emits Exact +
      `*.parent.tld` Wildcard tiers; skips loopback / IP literals /
      apex / leading-`www` / already-known hosts
- [x] `vetter-core::known_hosts` write API: atomic `write_file` +
      `add_host` (case-insensitive dedup) mirroring
      `matcher::loader`
- [x] Admin protocol: `MgmtRequest::{SuggestionsFor, AddRule,
      AddKnownHost}` and matching `MgmtResponse::{Suggestions,
      RuleAdded { auto_approved_ids }, KnownHostAdded}` over
      `vetter-admin.sock`
      ([vetter-core/src/wire/mod.rs](vetter-core/src/wire/mod.rs))
- [x] Daemon: shared mutable stores
      (`Arc<RwLock<AllowlistStore>>` /
      `Arc<RwLock<KnownHostsStore>>`) so admin handlers can persist
      + reload without restarting the daemon
      ([vetterd/src/lib.rs](vetterd/src/lib.rs))
- [x] `vetterd::suggestions` module: `add_allowlist_rule` writes
      YAML, reloads the store, and auto-resolves any pending
      requests the new rule covers (reason "auto-approved by newly
      added rule `<id>`"). `add_known_host` writes + reloads +
      refreshes pending signals via
      `PendingQueue::refresh_with(known_hosts)`. `suggestions_for`
      returns the picker payload for any pending or Allow-resolved
      id. ([vetterd/src/suggestions.rs](vetterd/src/suggestions.rs))
- [x] Popover: `Allowlist…` and `Trust host…` buttons on every
      pending and Allow-resolved card, picker sheets via
      `NSAlert + accessoryView`
      ([vetterd/src/runloop/popover.rs](vetterd/src/runloop/popover.rs),
      [vetterd/src/runloop/popover_picker.rs](vetterd/src/runloop/popover_picker.rs))
- [x] Integration coverage in
      [vetterd/tests/suggestions_admin.rs](vetterd/tests/suggestions_admin.rs):
      AddRule auto-approves matching pending + writes YAML +
      audits the auto-approve reason; non-covering AddRule leaves
      pending untouched; AddKnownHost does NOT auto-approve but
      flips `UnknownHost` signals away

## Phase 5.1 - Other improvements

- [x] add logo to the project, and use it for the menu bar icon
- [x] after an action is already approved, I want a way to identify the allowlist entry that approved it (if applicable), in case I need to remove an overbroad allowlist rule
      Auto-allow / auto-deny rows now carry the matcher's
      `(rule_id, scope)` attribution end-to-end:
      [`PolicyOutcome::Auto`](vetterd/src/policy.rs) plumbs it through
      `resolve_outcome` and into [`AuditEntry`](vetterd/src/audit.rs)
      as a new `rule_scope` field; `resolve_outcome` also pre-renders
      the §8.5 detail and pushes auto-decisions onto
      [`PendingQueue::record_auto`](vetterd/src/pending.rs) so the
      popover's Recent section surfaces auto-allows alongside human
      prompts. On the UI side, auto-allow Recent cards grow a new
      `▸ See approval reason` disclosure (sibling of `Show raw`) that
      names the rule + scope and exposes a **Revoke rule** button;
      revoke routes through a confirmation `NSAlert` and the new
      [`MgmtRequest::RemoveRule`](vetter-core/src/wire/mod.rs) admin
      call, which calls
      [`vetter_core::matcher::loader::remove_rule`](vetter-core/src/matcher/loader.rs)
      via [`vetterd::suggestions::remove_allowlist_rule`](vetterd/src/suggestions.rs).
      Past auto-allowed cards stay in Recent as a record of what was
      approved while the rule was live; only future requests see the
      change.
      ([vetterd/src/runloop/popover.rs](vetterd/src/runloop/popover.rs),
      [vetterd/src/runloop/popover_picker.rs](vetterd/src/runloop/popover_picker.rs))
- [x] if an action to be approved has a file input, I want some way to easily inspect that from the UI (e.g. click a button to open that file in a text editor or something?)
      Per-row **Open file** button (SF Symbol `arrow.up.right.square`)
      sits on every `FileRead` and `Body::FromFile` row in the popover
      card; clicking it routes through `[NSWorkspace sharedWorkspace]
      openURL:` to the user's default app. The button is suppressed
      when the path doesn't exist on disk so writes-to-be-created
      and dangling references stay button-free; `FileWrite` and
      `ProcessSpawn` rows deliberately remain untouched this round
      ([vetterd/src/runloop/popover_effects.rs](vetterd/src/runloop/popover_effects.rs)
      gains the `FileButtonFactory` plumbing,
      [vetterd/src/runloop/popover.rs](vetterd/src/runloop/popover.rs)
      adds the `openFileClicked:` selector + `file_paths` registry
      and `build_open_file_button` helper)
- [x] I am concerned about the stuff that `vet` adds to stdout, in case I want to pipe my curl output into another command like jq. Let's make sure all of our output is sent to `stderr` only? or some other solution? to avoid messing with such pipe-based commands
      (audit confirms `vet` writes only to stderr on the wrap / explain
      / fail-closed paths; `vet/tests/wrap_cli.rs::allow_path_passes_through_curl_stdout_unchanged`
      pins byte-perfect stdout passthrough across the full
      parse → daemon round-trip → `execvp` pipeline, with companion
      stdout-empty assertions in `vet/tests/explain.rs`; invariant
      documented atop [vet/src/wrap.rs](vet/src/wrap.rs) and
      [vet/src/explain.rs](vet/src/explain.rs))
- [x] play a sound when a new approval popover/banner is created, so I know an agent is waiting for approval
      New `notification_sound: bool` field on
      [`vetter_core::settings::Settings`](vetter-core/src/settings.rs)
      (defaults **on** — unlike `autostart`, this has no security
      implication). `MacNotifier::notify`
      ([vetterd/src/notifier/mac.rs](vetterd/src/notifier/mac.rs))
      re-reads the setting fresh on every banner it posts (same
      `NotifyHint::was_empty_before` coalescing gate as the banner
      itself) and passes it to
      [`runloop::post_notification`](vetterd/src/runloop/mod.rs),
      which sets `UNNotificationSound::defaultSound()` on the
      notification content — reusing the `Sound` permission already
      requested, and respecting the user's system Focus / per-app
      notification-sound settings. A **Play sound on new request**
      checkbox in the popover footer (alongside **Start at login**)
      toggles the setting directly (no OS API to converge, so no
      rollback beyond a failed settings write)
      ([vetterd/src/runloop/popover.rs](vetterd/src/runloop/popover.rs),
      [vetterd/src/runloop/mod.rs](vetterd/src/runloop/mod.rs)).
      Docs: [plans/MacOSApp.md § Notification sound](plans/MacOSApp.md#notification-sound).
- [x] Time-limited / session-scoped allowlist rules: `Rule` grows
      optional `expires_at` (Unix epoch seconds) and `sid` (POSIX
      session id) fields; `matcher::decide`/`matches_rule` take
      `now`/`caller_sid` and reject expired or session-mismatched
      rules. `peer_cred::stable_session_for` resolves a stable
      session id by walking the connecting process's ancestry to
      the nearest tty-anchored session (native `getsid`/ppid/tty
      lookups on Linux + macOS, no `ps` shelling), captured once per
      connection in `handle_connection` and threaded through
      `PromptSummary::peer_sid` / `AuditEntry::peer_sid`. Session
      rules are disk-persisted like any other rule (no separate
      store or wire message): `load_default()` partitions whatever
      it reads off disk into `store.session` based on the two new
      fields, and `allow_loader::add_rule` lazily prunes already-
      expired rules on every write. The popover's Allowlist… picker
      grows a "Duration:" radio group (15m / 1h / 4h / for this
      terminal session / Forever) that sets the two fields before
      calling the same, unchanged `add_allowlist_rule(User)`; a
      pure session-scoped pick also gets a 7-day backstop
      `expires_at` so lazy-prune eventually reaps it even if the
      terminal never comes back. `vet allow list` / `vet --explain`
      surface session rules for free.
      ([vetter-core/src/matcher/rule.rs](vetter-core/src/matcher/rule.rs),
      [vetter-core/src/matcher/decide.rs](vetter-core/src/matcher/decide.rs),
      [vetter-core/src/matcher/loader.rs](vetter-core/src/matcher/loader.rs),
      [vetter-core/src/peer_cred.rs](vetter-core/src/peer_cred.rs),
      [vetterd/src/lib.rs](vetterd/src/lib.rs),
      [vetterd/src/pending.rs](vetterd/src/pending.rs),
      [vetterd/src/runloop/popover_picker.rs](vetterd/src/runloop/popover_picker.rs),
      [plans/Overview.md §5](plans/Overview.md))

## Phase 5.2 — Curl parser file-effect gaps

Bugs and omissions in
[vetter-core/src/parsers/curl/](vetter-core/src/parsers/curl/) around
`Effect::FileRead` / `Effect::FileWrite` emission. Today the parser
covers `-o` / `-O` / `-J` (writes) and `-T` / `-d @file` (reads), but
several common flags fall through to `extras.unknown_long_flags` and
silently mis-vet. Spec: `plans/Overview.md` §8.4. Each item lands with
a fixture under
[vetter-core/tests/corpus/curl/](vetter-core/tests/corpus/curl/) and a
unit test in
[vetter-core/src/tests/parsers_curl_state.rs](vetter-core/src/tests/parsers_curl_state.rs).

- [x] Reject `-K` / `--config <file>` with `ParseError::Other` until we
      recursively parse the referenced config (today it lands in
      `extras.unknown_long_flags`, so a config containing `output =
      /etc/passwd` or `url = https://attacker/` is invisible)
- [x] Reject `-:` / `--next` with `ParseError::Other` until the parser
      can split one invocation into multiple `HttpRequest` effects
      (today only the first request is rendered, the rest are
      mis-vetted)
- [x] Resolve relative paths against `EnvSnapshot::cwd` inside the
      parser before constructing `FileRead` / `FileWrite`, so `-o
      ./out`, `-O thing.tgz`, `-d @./payload.json`, `-T rel` no longer
      produce spurious `FileOutsideCwd` / `FileReadOutsideCwd` signals
      via `signals::path_is_inside` (which requires absolute paths).
      `parse_argv` now takes the agent cwd, joins it onto every
      relative `FileRead` / `FileWrite` path (collapsing `.`/`..`
      segments), and threads `parsed.cwd = env.cwd` so the CLI side
      (`vet --explain`, `vet curl …`) gets the same signal coverage
      the daemon already had
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/src/parsers/curl/mod.rs](vetter-core/src/parsers/curl/mod.rs),
      [vetter-core/src/tests/parsers_curl_state.rs](vetter-core/src/tests/parsers_curl_state.rs))
- [x] Reject `-F` / `--form` / `--form-string` with `ParseError::Other`
      until we model multipart precisely (today they fall through to
      `extras.unknown_short_flags` and silently mis-vet `-F
      file=@/etc/passwd`). Multipart curl is uncommon in agent
      workflows; failing closed beats failing open. Same pattern as
      `--config` / `-K` and `--next` / `-:` already use
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/src/parsers/curl/flags.rs](vetter-core/src/parsers/curl/flags.rs))
- [x] Distinguish `-b @file` / `--cookie @file` (`FileRead` of a
      credential file) from `-b "k=v"` (header-only); inline pairs
      surface in `extras.cookies_inline` for visibility, file
      occurrences emit one `FileRead` each (with `cwd` resolution)
      and `-b @-` fails closed via `ParseError::StreamingUnsupported`
      ([vetter-core/src/parsers/curl/flags.rs](vetter-core/src/parsers/curl/flags.rs),
      [vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/cookie_file.argv](vetter-core/tests/corpus/curl/cookie_file.argv),
      [vetter-core/tests/corpus/curl/cookie_inline.argv](vetter-core/tests/corpus/curl/cookie_inline.argv))
- [x] Emit `FileWrite` for `-c <file>` / `--cookie-jar <file>` with
      `WriteSource::RemoteHttp { url }`; only the most-recent
      occurrence wins, the path is resolved against `cwd`, and the
      jar write coexists with any `-o`/`-O` body write
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/cookie_jar.argv](vetter-core/tests/corpus/curl/cookie_jar.argv))
- [x] Recognise the seven curl client-TLS flags (`--cert`, `--key`,
      `--cert-type`, `--key-type`, `--pass`, `--pubkey`, `--engine`):
      emit `FileRead` for the path-bearing trio (`--cert`, `--key`,
      `--pubkey`) with `cwd` resolution, push a new
      `SignalKind::ClientCertificate` (Danger) for `--cert` / `--key`
      alongside the existing `CacertOverride`, surface
      `--cert-type` / `--key-type` / `--engine` plus a
      `cert_password_supplied` boolean in `extras`, and never echo the
      `--pass` value (or the `:password` suffix on `--cert`) anywhere
      in the parsed command surface
      ([vetter-core/src/parsers/curl/flags.rs](vetter-core/src/parsers/curl/flags.rs),
      [vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/src/signals/mod.rs](vetter-core/src/signals/mod.rs),
      [vetter-core/src/render/mod.rs](vetter-core/src/render/mod.rs),
      [vetter-core/tests/corpus/curl/client_cert.argv](vetter-core/tests/corpus/curl/client_cert.argv))
- [x] Emit `FileWrite` for `-D` / `--dump-header <file>` (treating
      `-` / stdout as no effect)
      ([vetter-core/src/parsers/curl/flags.rs](vetter-core/src/parsers/curl/flags.rs),
      [vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/dump_header.argv](vetter-core/tests/corpus/curl/dump_header.argv))
- [x] Emit `FileWrite` for `--trace` / `--trace-ascii <file>`,
      treating the special `-` (stdout) and `%` (stderr) values as
      non-file
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/trace_to_file.argv](vetter-core/tests/corpus/curl/trace_to_file.argv))
- [x] Emit `FileRead` for `--write-out` / `-w` with a leading `@`
      (e.g. `-w @fmt.txt`); bare format string stays informational;
      `@-` (stdin format) fails closed via `StreamingUnsupported`
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/write_out_stdin.argv](vetter-core/tests/corpus/curl/write_out_stdin.argv))
- [x] Emit `FileWrite` for `--etag-save <file>` and `FileRead` for
      `--etag-compare <file>`
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/etag_roundtrip.argv](vetter-core/tests/corpus/curl/etag_roundtrip.argv))
- [x] Honour `--output-dir <dir>` when constructing the
      `FileWrite.path` for `-o` / `-O` / `-J`; absolute `-o /abs/...`
      paths still ignore the prefix (matching curl's real behaviour),
      relative paths get joined onto the `output-dir` value before
      `cwd` resolution
      ([vetter-core/src/parsers/curl/flags.rs](vetter-core/src/parsers/curl/flags.rs),
      [vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/output_dir.argv](vetter-core/tests/corpus/curl/output_dir.argv))
- [x] Honour `--no-clobber` by flipping `FileWrite.overwrite` to
      `false` (was hard-coded `true` in `build_file_write`)
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/no_clobber.argv](vetter-core/tests/corpus/curl/no_clobber.argv))
- [x] Parser-pushed signal `SignalKind::RemoteHeaderName` (Warn) for
      `-J` / `--remote-header-name`, recording that the on-disk
      filename comes from `Content-Disposition` and the surfaced
      path is a URL-basename placeholder
      ([vetter-core/src/signals/mod.rs](vetter-core/src/signals/mod.rs),
      [vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/remote_header_name_placeholder.argv](vetter-core/tests/corpus/curl/remote_header_name_placeholder.argv))
- [x] Surface `--create-dirs` as `SignalKind::CreateDirs` (Warn) so
      the approver sees that writes can land arbitrarily deep below
      `--output-dir` / `-o`'s parent
      ([vetter-core/src/signals/mod.rs](vetter-core/src/signals/mod.rs),
      [vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/create_dirs.argv](vetter-core/tests/corpus/curl/create_dirs.argv))
- [x] Resolved by failing closed: `build_body` now rejects any `-d` /
      `--data*` invocation that mixes inline literals with `@file`
      chunks, or supplies multiple `@file` chunks, with
      `ParseError::Other` (matching the existing `--config` / `--next`
      / `-F` convention). Curl `&`-joins file contents into the wire
      body, so silently surfacing only the inline bytes + the first
      `FileRead` mis-vetted both the body the approver saw and the
      file reads the audit log recorded. Agents that genuinely need
      multi-source bodies should pre-concatenate into one file and
      pass `-d @combined`. Widening `Body` to a multi-source variant
      stays available if a future parser (`wget`, `httpie`) needs it.
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/data_mixed_inline_and_file.argv](vetter-core/tests/corpus/curl/data_mixed_inline_and_file.argv),
      [vetter-core/tests/corpus/curl/data_multi_at_file.argv](vetter-core/tests/corpus/curl/data_multi_at_file.argv))
- [x] Decided multi-URL handling: reject with `ParseError::Other`.
      Agents must issue one `vet curl` per URL so each request gets
      its own approval. Corpus fixture `multi_url.argv` + unit tests
      `multiple_urls_rejected` / `multiple_urls_with_output_rejected`
      pin the behaviour
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/tests/corpus/curl/multi_url.argv](vetter-core/tests/corpus/curl/multi_url.argv))

## Phase 5.3 — Safe-paths layer  `[ ] not started`

Design: [plans/FilePaths.md](plans/FilePaths.md).

Models file paths as a **separate, command-agnostic layer** rather
than as clauses on the rule allowlist. A new `safe-paths.yaml`
(layered identically to `allowlist.yaml`: built-in / user / project
/ session) carries `allow.read` / `allow.write` / `deny.read` /
`deny.write` lists; the daemon evaluates every `Effect::FileRead`
/ `Effect::FileWrite` against it independently of the rule
allowlist's `decide()`, then combines the two outcomes per
[FilePaths.md §5.2](plans/FilePaths.md). The cross-cutting framing
means a single safe-paths entry covers `~/Downloads/**` for every
parser (curl, wget, httpie, …) without duplicating policy on each
rule. The
`RuleWhen.file_write` / `RuleWhen.file_read` clauses leave the rule
schema entirely. Adds a built-in baseline of safe directories (so
common scratch and cache writes auto-allow) plus a built-in deny
list for sensitive credential paths. Replaces `FileOutsideCwd` /
`FileReadOutsideCwd` with `UnknownWritePath` / `UnknownReadPath` /
`DeniedWritePath` / `DeniedReadPath`. Closes the existing backlog
item "Allowlist suggestions over `Effect::FileWrite` /
`Effect::FileRead`" by wiring the new suggestion engine into the
popover's `Allow path…` picker.

### PR A — `safe_paths` module + decision plumbing

- [ ] New `vetter-core::safe_paths` module mirroring
      [vetter-core/src/known_hosts.rs](vetter-core/src/known_hosts.rs):
      `SafePathEntry`, `SafePathsFile`, `SafePathsStore`,
      `BUILTIN_SAFE_PATHS`, `BUILTIN_DENY_SAFE_PATHS`,
      `load_default`, `write_file`, `add_entry`,
      `user_safe_paths_path`. Project discovery reuses
      `matcher::loader::discover_project_root`. `0600` writes via
      `fs_secure::persist_at_mode` for parity with allowlist /
      known-hosts.
- [ ] Minimal built-in entries from
      [FilePaths.md §7.1 / §7.2](plans/FilePaths.md): mechanical
      allow list (`${cwd}/**`, `${TMPDIR}/**`, `/tmp/**`,
      `/private/tmp/**`, `/var/folders/**`, `/dev/{null,stdout,stderr}`),
      single deny entry (`${HOME}/.vet/**` write). Expand
      `${cwd}` per-request and `${TMPDIR}` at load time; entries
      whose env var is unset are silently dropped and reported by
      `vet doctor`. Deliberately *not* baking in `~/.ssh/**`,
      `~/.aws/credentials`, `/etc/**`, `~/Downloads/**`, etc. —
      those live in the starter template (PR C) and are
      user-authored, not built-in.
- [ ] Tilde (`~/`) expansion for user-authored entries; reject
      unexpanded `${...}` templates outside built-in scope so the
      same `safe-paths.yaml` doesn't evaluate differently per
      invocation.
- [ ] Delete `RuleWhen.file_write`, `RuleWhen.file_read`,
      `FileWriteClause`, `FileReadClause` from
      [vetter-core/src/matcher/rule.rs](vetter-core/src/matcher/rule.rs);
      delete the corresponding branches in
      [vetter-core/src/matcher/decide.rs](vetter-core/src/matcher/decide.rs)
      `matches_rule`. Update tests + snapshots; surface a clear
      load-time error if an existing YAML still carries those
      keys.
- [ ] Plan-doc touch-ups landing with the same PR:
      [plans/TestingPlan.md](plans/TestingPlan.md) §3 file-effect
      fixtures + §2.5 / §2.6 multi-effect rule paragraphs,
      [plans/RepoMap.md](plans/RepoMap.md) §2 / §3 schema rows,
      [plans/ThreatModel.md](plans/ThreatModel.md) "File-rule
      amplifier" → "Safe-paths amplifier" reframe.
- [ ] `SignalKind::{UnknownWritePath, UnknownReadPath,
      DeniedWritePath, DeniedReadPath}` in
      [vetter-core/src/signals/mod.rs](vetter-core/src/signals/mod.rs);
      retire emission of `FileOutsideCwd` / `FileReadOutsideCwd`
      and mark them `#[deprecated]`. Signals are descriptive of
      the safe-paths verdict (so the popover effect-row pill
      explains why the path is yellow / red); the actual
      gating happens in the decision combinator below.
- [ ] `safe_paths::decide_file_paths(parsed, store) ->
      Vec<(EffectIdx, PathOutcome)>` returning per-effect
      Allowed / Unknown / Denied. Combine with
      `matcher::decide()` per the table in
      [FilePaths.md §5.2](plans/FilePaths.md): file Denied →
      Deny; file Unknown + rule Allow → Prompt; otherwise rule
      decision wins.
- [ ] Wire from [vet/src/explain.rs](vet/src/explain.rs) and
      [vetterd/src/policy.rs](vetterd/src/policy.rs); plumb the
      per-effect `Vec<(EffectIdx, PathOutcome)>` attribution into
      `PolicyOutcome::Auto` and the new `file_paths` field on
      [vetterd/src/audit.rs](vetterd/src/audit.rs)'s
      `AuditEntry`.
- [ ] `vet doctor` rows: `safe-paths (user)`,
      `safe-paths (project)`, built-in allow / deny entry counts
      (with `dropped M` when env vars are unset).

### PR B — Suggestion engine + popover picker

- [ ] `safe_paths::suggest_for(&ParsedCommand) ->
      Vec<PathSuggestion>` per
      [FilePaths.md §8.1](plans/FilePaths.md): Exact / Dir /
      ProjectRoot tiers per uncovered effect, each carrying
      `op: SafePathOp::{Read, Write}`. Suppress tiers that
      overlap the built-in deny list.
- [ ] Wired into
      [vetterd/src/suggestions.rs](vetterd/src/suggestions.rs)
      `suggestions_for`.
- [ ] `MgmtRequest::AddSafePath { scope, op, entry }` /
      `MgmtResponse::SafePathAdded { auto_approved_ids }` in
      [vetter-core/src/wire/mod.rs](vetter-core/src/wire/mod.rs);
      handler in
      [vetterd/src/lib.rs](vetterd/src/lib.rs) calls
      `safe_paths::add_entry`, reloads the store, and
      auto-resolves any pending request whose previously-Unknown
      effect now resolves to Allowed (mirrors the existing
      `add_allowlist_rule` auto-approve flow).
- [ ] `Allow path…` button on `FileRead` / `FileWrite` rows in
      the macOS popover
      ([vetterd/src/runloop/popover_effects.rs](vetterd/src/runloop/popover_effects.rs));
      picker sheet reuses
      [vetterd/src/runloop/popover_picker.rs](vetterd/src/runloop/popover_picker.rs);
      button suppressed when the path overlaps the built-in
      deny list.
- [ ] Integration tests in `vetterd/tests/` for the suggestion
      flow: `AddSafePath` auto-approves matching pending +
      writes YAML + audits the auto-approve reason;
      built-in-deny overlap suppresses the picker; Unknown path
      stays pending until the operator approves once or adds an
      entry.

### PR C — Authorship ergonomics (CLI + starter template)

- [ ] `vet allow add --safe-read <path>` /
      `--safe-write <path>` /
      `--safe-deny-read <path>` / `--safe-deny-write <path>`
      shims in [vet/src/allow.rs](vet/src/allow.rs) (wraps
      `safe_paths::add_entry`).
- [ ] `vet allow list --safe-paths` lists every layer's entries
      with scope attribution; surface the single built-in deny
      entry so the operator can see the floor.
- [ ] Ship `safe-paths.starter.yaml` under
      `vetter-core/resources/` containing the curated set from
      [FilePaths.md §7.3](plans/FilePaths.md) — opinionated
      defaults for `~/Downloads/**`, `~/.cache/**`, `/etc/ssl/**`,
      and the deny-recommendations for `~/.ssh/**`,
      `~/.aws/credentials`, `~/.gnupg/**`, `~/.kube/config`,
      `~/.netrc`, `/etc/**`. Not loaded by the daemon.
- [ ] `vet allow init-safe-paths` subcommand: copies the starter
      template to `~/.vet/safe-paths.yaml`, expands `${HOME}` /
      `${XDG_CACHE_HOME}` once at copy time so the resulting
      file is portable, refuses to overwrite an existing file
      without `--force`, supports `--print` for stdout review.
- [ ] `vet doctor` first-run hint: when `~/.vet/safe-paths.yaml`
      is missing, surface an `INFO` row pointing at
      `vet allow init-safe-paths` and the popover picker.

---

## Phase 6 — Ubuntu support  `[ ] not started`  (v0.2 milestone)

Roadmap: [plans/Overview.md](plans/Overview.md) §7 ("Linux"), §11.
Operational notes + manual smoke procedure:
[plans/LinuxApp.md](plans/LinuxApp.md). Distribution flow:
[plans/Release.md](plans/Release.md) §"Linux / Launchpad PPA".

End-state: `sudo add-apt-repository ppa:blevinstein/vetter && sudo
apt-get install vetter` puts `vet` + `vetterd` on `$PATH`, starts
the daemon under `systemd --user`, lights a tray icon, and routes
prompt-class requests through D-Bus notifications + a GTK4 popover
window — same separate-channel guarantee as macOS, no TTY prompt.

### PR 1 — Linux daemon spine + admin-CLI approve/reject

- [ ] Refactor [vetterd/src/runloop/](vetterd/src/runloop/) into
      `runloop/mac/` (move-only, keep the diff reviewable). Add a
      thin `runloop/mod.rs` that re-exports the right submodule
      per `cfg(target_os)`.
- [ ] Add `PlatformDriver::Glib` to
      [vetterd/src/lib.rs](vetterd/src/lib.rs); `run_with_glib`
      mirrors `run_with_appkit`, accept-loop on background thread.
- [ ] Stub `notifier::linux::LinuxNotifier` (no-op `notify`,
      session-bus availability check in `install`); flip
      `default_kind()` in
      [vetterd/src/notifier/mod.rs](vetterd/src/notifier/mod.rs)
      to `"linux"` on Linux.
- [x] Add `MgmtRequest::Resolve { id, decision, reason }`
      + matching `MgmtResponse::Resolved { id, decision }` to
      [vetter-core/src/wire/mod.rs](vetter-core/src/wire/mod.rs);
      handle in
      [vetterd/src/lib.rs::handle_admin_request](vetterd/src/lib.rs).
      (Named `Resolve`/`Resolved`, not `ResolvePending`/
      `PendingResolved`, to match `PendingQueue::resolve`.) The
      daemon expands a unique ULID *prefix* to the full id, so the
      match is atomic against its own pending map; unknown,
      already-resolved, and ambiguous ids are all errors.
- [x] `vet daemon approve <id>` and `vet daemon reject <id>`
      subcommands in [vet/src/daemon.rs](vet/src/daemon.rs).
      `--reason <str>` is on `reject` only, per
      [plans/LinuxApp.md](plans/LinuxApp.md) §6a; the wire field
      carries a reason for either decision if `approve` ever wants
      one.
- [ ] CI: extend `.github/workflows/ci.yml` test job with
      `apt-get install -y libgtk-4-dev libdbus-1-dev`.

### PR 2 — D-Bus notifications via zbus

- [x] Real `LinuxNotifier::notify` against
      `org.freedesktop.Notifications`; subscribe to
      `ActionInvoked`, `NotificationClosed` and `ActivationToken`;
      route to `PendingQueue::resolve` (mirroring
      `runloop/mac/mod.rs::did_receive_response`).
      `ActivationToken` is parsed but unused until 6d has a window
      to raise.
- [x] Coalesce after first banner per existing
      `NotifyHint::was_empty_before`.
- [x] Capability fallback: when `GetCapabilities` lacks
      `actions`, post a body-only notification and rely on the
      tray (PR 3) / `vet daemon approve` (PR 1) to resolve.
      Capabilities are re-queried on `NameOwnerChanged` so a
      notification-daemon restart is picked up.
- [ ] Mock `org.freedesktop.Notifications` server in
      `vetterd/tests/notifier_linux_dbus.rs` for E2E coverage of
      action routing. **Prototyped and proven during 6b** but not
      landed: a Python fake server under `dbus-run-session` drove
      the whole loop (Notify → `ActionInvoked` → resolve →
      `CloseNotification`), including the no-`actions` degradation
      and a mid-run server restart. Automating it needs a decision
      about a Python dependency in CI, which belongs with the PR-6
      CI work rather than here.

### PR 3 — Tray (StatusNotifierItem)

- [x] `runloop/linux/status_item.rs` using `ksni`: shield icon,
      pending-count badge, "Open vetter window…" / "Pending: N"
      / "Quit Vetter" menu.
- [x] Wire the queue change-listener to call `ksni::Handle::update`
      on every state flip (mirrors
      `runloop/mac/status_item.rs::set_pending_count`).
- [x] Hicolor SVG asset under `vetterd/resources/icons/` with the
      same shield silhouette as macOS.

### PR 4 — GTK4 popover window

- [ ] `runloop/linux/popover.rs` (and per-section helpers
      `popover_url.rs`, `popover_pills.rs`, `popover_effects.rs`,
      `popover_picker.rs` — same module split as macOS so the
      port stays diff-reviewable).
- [ ] Translate the `Style` SGR taxonomy from
      [plans/ApprovalUI.md](plans/ApprovalUI.md) §"Body colouring"
      into `pango::AttrList`. Add a port of the macOS
      `popover_attr.rs` ANSI parser.
- [ ] Reuse `vetter-core::suggest` and `vetterd::suggestions`
      unchanged for the Allowlist… / Trust host… picker sheets;
      replace `NSAlert + accessoryView` with `gtk::Dialog` +
      `gtk::Box` of radio buttons.
- [ ] Tray click opens the popover window; double-click on a
      notification body opens it scrolled to the matching id
      (mirrors macOS `focused_id` flow).

### PR 5 — `.deb` packaging + systemd user service

- [ ] `[package.metadata.deb]` block in
      [vetterd/Cargo.toml](vetterd/Cargo.toml) describing
      maintainer, description, depends, assets, and
      `maintainer-scripts` directory.
- [ ] `vetterd/resources/vetter.service` (systemd user unit;
      `Environment=VETTERD_NOTIFIER=linux`,
      `WantedBy=default.target`).
- [ ] `vetterd/resources/vetter.desktop` (autostart entry under
      `/etc/xdg/autostart/`). Note: this is the Linux mirror of
      the macOS "Start at login" feature. The systemd
      `WantedBy=default.target` user unit above is what actually
      brings the daemon up at every login; the `.desktop` file is
      only needed for users who run vetterd outside a systemd-
      session manager. We do **not** plan to surface the
      `Settings::autostart` toggle on Linux: opting in is the
      install-time default (`postinst` enables the user unit),
      and revocation goes through `systemctl --user disable
      vetter.service`.
- [ ] `vetterd/resources/postinst` →
      `systemctl --user --global enable vetter.service`.
- [ ] `tools/release-deb.sh`: `cargo build --release` →
      `cargo deb --no-build` → `debuild -S -sa -k$GPG_SIGN_KEY`.
- [ ] CI smoke job: `cargo deb` then
      `dpkg-deb --contents target/debian/*.deb` against an
      expected manifest.

### PR 6 — Launchpad PPA + Release.md update + README

- [ ] Create `ppa:blevinstein/vetter` on Launchpad (manual; one-
      time per maintainer).
- [ ] First upload via `dput vetter-ppa target/source-package/
      vetter_*_source.changes`; verify per-arch builds succeed
      for jammy and noble.
- [ ] Update `vet doctor` to surface Linux-specific rows: D-Bus
      session bus reachable, notification daemon name + version,
      StatusNotifierWatcher present, systemd user service status,
      package source (`dpkg -S` lookup against the binary path).
- [ ] Update [README.md](README.md) status section; add the
      Ubuntu install block (parallel to the existing macOS one).
- [ ] Update [TODO.md](TODO.md) (this file): flip Phase 6 boxes
      to `[x]` and the phase tag to `[x] done` once PR 6 lands.

---

## Build health  `[x] done`

Items that make `cargo clippy` / `cargo test` red at `main`. These
jump the queue regardless of which phase is in flight: a red baseline
hides the next real regression.

- [x] `cargo clippy --workspace --all-targets -- -D warnings` failed at
      `main` on two unused imports —
      `use std::os::unix::fs::PermissionsExt as _;` in
      [vetter-core/src/tests/fs_secure.rs](vetter-core/src/tests/fs_secure.rs):3
      and
      [vetter-core/src/tests/socket_dir.rs](vetter-core/src/tests/socket_dir.rs):3.
      Both test files do use `PermissionsExt` methods, but each is a
      `#[path]`-included `mod tests` whose `use super::*;` already
      pulls the trait in from the parent module's own import
      ([fs_secure.rs](vetter-core/src/fs_secure.rs):31,
      [socket_dir.rs](vetter-core/src/socket_dir.rs):16) — so the
      explicit import in the test file is redundant, not the usage.
      Fix is two line deletions; it stays correct only while both
      parents keep importing the trait, so delete the child import,
      not the parent's. Fails in **both** feature configurations, so
      CI is red independently of any in-flight phase work. Found
      while landing Phase 6a (2026-09-06), which touched neither
      file. **Fixed 2026-09-06**: both child imports deleted, parents
      left alone; `fmt`, both clippy configurations, and
      `cargo test --workspace --all-features` (637 passed) all green.

---

## Hardening — pre-release ship-blockers

These must close before the first public Homebrew release. None are
blocked by Phase 4 / Phase 5; pick them up in any order. Per-threat
detail in [plans/ThreatModel.md](plans/ThreatModel.md); per-task code
site noted inline.

### Already shipped

- [x] Write `plans/ThreatModel.md` (assets, attackers, in-scope /
      out-of-scope, residual risks) — landed in PR #4
- [x] Peer credential check on `vetterd` accept (`SO_PEERCRED` /
      `getpeereid`); reject connections whose uid doesn't match the
      daemon's — landed in PR #5 (T1 partial)
- [x] Drop `parsed` from `VetRequest`; daemon re-parses argv before
      matcher runs (T2). Wire bumped to v2; client / daemon /
      audit-log all re-derive command from `argv[0]`
- [x] `serde(deny_unknown_fields)` on `VetRequest` / `VetDecision`
      (already in place; new wire-format test enforces that legacy
      `parsed` payloads are rejected)
- [x] Expand `vet doctor` checks: socket perms, audit dir writable,
      allowlist parses, daemon reachable
      ([vet/src/doctor.rs](vet/src/doctor.rs) walks the
      pidfile → live-PID → socket → peer-cred state machine and
      reports OK/WARN/ERROR/INFO/SKIP per row; exit 78 on any error)
- [x] Real code-signing rows in `vet doctor`: per-binary
      `code signing (vet)` / `code signing (vetterd)` shell out to
      `codesign --verify --strict` + `codesign -d -vv` and surface
      ad-hoc / Developer-ID / hardened-runtime state; the bundle
      row uses `xcrun stapler validate` + `spctl --assess` to
      flag un-notarised / un-stapled `.app`s. Only
      `tools/release.sh`-built bundles report OK end-to-end; the
      cargo + `tools/build-app.sh` dev paths surface as WARN
      ([vet/src/doctor.rs](vet/src/doctor.rs))

### H1 — Daemon protocol & process hardening  `[ ] not started`

Closes the rest of T1, T3, and T4. ThreatModel.md §"Sequencing" lists
these in smallest-blast-radius-first order; same order here.

- [x] Read deadline on `vetterd`'s request frame + cap inflight
      workers (default 16, configurable via `VETTERD_MAX_INFLIGHT`,
      accept-and-immediately-close above the cap) — ThreatModel T3
      ([vetterd/src/lib.rs](vetterd/src/lib.rs) — `max_inflight_from_env`,
      `InflightGuard`, per-connection `set_read_timeout` /
      `set_write_timeout` of 5 s in `handle_connection` and
      `run_admin_loop`)
- [x] PID attestation on connect (`vetterd` holds an `fcntl(F_SETLK,
      F_WRLCK)` on the pidfile for its full lifetime; `vet` reads
      the locker via `F_GETLK` and asserts it equals the connected
      peer PID — ThreatModel T1 sequencing #1)
      ([vetter-core/src/pidfile.rs](vetter-core/src/pidfile.rs) —
      `PidFileLock` / `acquire` / `read_locker_pid`,
      [vetter-core/src/peer_cred.rs](vetter-core/src/peer_cred.rs) —
      `peer_pid` for Linux `SO_PEERCRED` + macOS `LOCAL_PEERPID`,
      [vet/src/wrap.rs](vet/src/wrap.rs) — `round_trip` cross-checks
      `peer_pid` against `read_locker_pid` before sending the
      request frame)
- [x] Resolve `argv[0]` to a real path / inode before parser dispatch
      so `ln /bin/bash /tmp/curl && vet /tmp/curl …` can't route to
      the wrong parser — ThreatModel T4
      ([vetter-core/src/parsers/mod.rs](vetter-core/src/parsers/mod.rs) —
      `resolve_for_dispatch` / `ResolvedCommand` / `ResolveError`,
      hybrid `(dev,ino)` match against `which <parser_name>` plus a
      trusted-install-dir fallback (`$VETTER_PARSER_TRUSTED_DIRS`
      colon-separated extension);
      [vet/src/wrap.rs](vet/src/wrap.rs) and
      [vet/src/explain.rs](vet/src/explain.rs) route through the
      resolver and `wrap.rs` execs `resolved.resolved_path` with
      `arg0(&argv[0])` so vetting and exec bind to the same inode;
      [vet/tests/argv0_spoof.rs](vet/tests/argv0_spoof.rs) end-to-end
      refusal coverage)
- [x] `FD_CLOEXEC` on daemon + client sockets and the audit fd; test
      that the exec'd child inherits only 0/1/2 (no leaked daemon fd
      survives the `execvp` into the wrapped command)
      Closed by inspection rather than code. The contract is already
      held by construction on every supported platform:
      (a) Rust std opens every fd CLOEXEC by default —
      `OpenOptions::open` uses `O_CLOEXEC`, `UnixStream::connect` and
      `UnixListener::bind` use `SOCK_CLOEXEC` (Linux) or
      `fcntl(F_SETFD)` post-`socket()` (macOS), and
      `UnixListener::accept` uses `accept4(SOCK_CLOEXEC)` on Linux /
      `fcntl(F_SETFD)` post-`accept()` on macOS — so every fd
      `vetter-core` / `vet` / `vetterd` open via the standard library
      already carries the bit;
      (b) `vet`'s only daemon-touching fds (the `UnixStream` to
      `vetter.sock` plus the read-only pidfile probe in
      `pidfile::read_locker_pid`) are scoped to
      `vet::wrap::round_trip` — both go out of scope and `Drop`'s
      `close(2)` runs before `Command::exec` is reached, so even
      without CLOEXEC the wrapped child cannot inherit them;
      (c) `vetterd` itself never `exec`s a child today, so the
      audit-log fd / socket fds it holds for its lifetime have
      nothing to leak into. A defensive `set_cloexec(fd)` helper plus
      regression test would only document an invariant std already
      enforces; the cost (the test would have to fight bash's
      script-host fd 255, dev-shell-inherited terminal IPC sockets at
      fd 3/4, and platform `/dev/fd/` listing semantics) outweighs
      the zero-bit security gain. Re-open if a future `vetterd`
      change starts spawning helper subprocesses (codesign /
      hash-recompute / etc.) — at that point the audit-log fd
      genuinely could leak and the explicit CLOEXEC becomes
      load-bearing rather than belt-and-suspenders.
- [x] Verify socket parent dir owner + mode before `vet` connects;
      refuse on mismatch. (Daemon enforces `0700` at bind; the
      client-side check is the residual T1 gap — peer-cred largely
      neutralises it but the check is cheap.)
      `vetter_core::socket_dir::verify_socket_parent` checks parent
      dir owner UID and mode (rejects `& 0o077 != 0`) before
      `UnixStream::connect`; new `WireError::SocketDirInsecure`
      variant surfaces a clear error message
      ([vetter-core/src/socket_dir.rs](vetter-core/src/socket_dir.rs),
      [vet/src/wrap.rs](vet/src/wrap.rs))
- [ ] Same-UID admin-socket write hardening: `MgmtRequest::AddRule`
      and `AddKnownHost` on `vetter-admin.sock` currently trust any
      same-UID caller (peer-cred passes by definition). A malicious
      same-UID process can call `AddRule { scope: User, … }` to
      persist an arbitrary rule into `~/.vet/allowlist.yaml` —
      bypassing the popover's `Allowlist…` confirmation flow — and
      every future `vet curl …` matching that rule then auto-allows
      with no human prompt. `AddKnownHost` similarly silences the
      `UnknownHost` signal (T5 partial mitigation) for
      attacker-chosen hosts. Two viable closes: (a) demote these
      mutations off the admin socket and call
      [`vetterd::suggestions::add_allowlist_rule`](vetterd/src/suggestions.rs)
      directly from the in-process popover, leaving the admin
      socket read-only; or (b) require an explicit human-confirmed
      modal naming the calling process before persisting. The
      `vet allow add` CLI already writes the YAML directly via
      `vetter_core::matcher::loader` and does not depend on the
      admin-socket path. Also worth pairing with peer-cred on the
      main `accept_loop` (currently absent — a same-UID caller can
      submit fake `VetRequest`s for audit-log poisoning / phishing
      prompts) for parity.
- [x] Explicit `0600` mode on audit-log, allowlist, and known-hosts
      writes — ThreatModel T8. New `vetter_core::fs_secure` helper
      (`create_dir_secure` / `persist_at_mode`) consumed by
      `vetterd::audit::AuditLog::open`,
      `vetter_core::matcher::loader::write_file`,
      `vetter_core::known_hosts::write_file`, and
      `vetter_core::settings::write_to` (refactored onto the same
      helper) so every vetter-owned writer lands its destination at
      mode `0600` and any parent dir it creates at mode `0700`.
      `vet doctor` adds `vetter dir`, `known-hosts (user)`, and
      `known-hosts (project)` rows and overlays a `WARN` (with a
      `chmod 0600 …` repair hint) on the existing `audit log` /
      `allowlist (*)` rows whenever the on-disk file is wider than
      `0600` or its parent dir is wider than `0700`. Existing files
      we did not create on this run are deliberately *not* silently
      chmodded.
      ([vetter-core/src/fs_secure.rs](vetter-core/src/fs_secure.rs),
      [vetterd/src/audit.rs](vetterd/src/audit.rs),
      [vetter-core/src/matcher/loader.rs](vetter-core/src/matcher/loader.rs),
      [vetter-core/src/known_hosts.rs](vetter-core/src/known_hosts.rs),
      [vetter-core/src/settings.rs](vetter-core/src/settings.rs),
      [vet/src/doctor.rs](vet/src/doctor.rs))
- [ ] Env-override hardening — ThreatModel T6. `$VETTERD_SOCKET` /
      `$VETTERD_PIDFILE` / `$VETTER_AUDIT_LOG` /
      `$VETTER_PARSER_TRUSTED_DIRS` are all agent-inheritable env
      vars; an attacker that controls `vet`'s env can bind a fake
      socket + self-lock a fake pidfile and auto-allow every
      request (both the UID check and the PID attestation are
      satisfied by construction). `$VETTER_PARSER_TRUSTED_DIRS`
      separately lets an attacker bless an agent-writable directory
      so `/tmp/evil/curl` passes the T4 inode-resolver's trust-
      dir arm. Close: (a) treat `$VETTERD_*` / `$VETTER_*`
      overrides as dev-only — refuse to start unless a sentinel
      file under `~/.vet/` opts in; (b) refuse
      `$VETTER_PARSER_TRUSTED_DIRS` entries whose canonical parent
      is not owned by `self_uid` and not `0755`-or-tighter; (c)
      add a client-side parent-dir owner + mode check on the
      resolved socket path (already open as a residual T1 item
      above — pair these fixes).

### H2 — Render trust & input safety  `[ ] not started`

The renderer is the surface the user trusts before clicking Approve.
Argv-controlled bytes (URLs, header values, paths) must not be able
to inject control sequences that hide or fake content, and the
parser/wire layers must not panic on malformed input.

- [x] Sanitise ANSI / C0 control bytes in renderer output (header
      values, URLs, paths, argv echo) before any TTY or popover
      write. Corpus test with embedded ANSI cursor moves, RTLO
      (U+202E), zero-width chars, and bare CR / BS so a malicious
      argv can't repaint the screen or hide a path segment.
      `vetter_core::render::sanitize_for_display` replaces every
      C0 / DEL / C1 / bidi-override / zero-width / BOM byte with a
      visible `<U+XXXX>` placeholder; called at every untrusted-
      chunk site in the §8.5 renderer
      ([vetter-core/src/render/mod.rs](vetter-core/src/render/mod.rs))
      and in the macOS popover's parallel render surfaces
      ([vetterd/src/runloop/popover_url.rs](vetterd/src/runloop/popover_url.rs),
      [vetterd/src/runloop/popover_effects.rs](vetterd/src/runloop/popover_effects.rs),
      [vetterd/src/runloop/mod.rs](vetterd/src/runloop/mod.rs)
      `post_notification` for the banner body). Coverage:
      per-class unit tests in
      [vetter-core/src/tests/render_escape.rs](vetter-core/src/tests/render_escape.rs),
      property tests in
      [vetter-core/src/tests/render.rs](vetter-core/src/tests/render.rs)
      (header ANSI / RTLO / zero-width / forged-newline), the
      [embedded_ansi_header](vetter-core/tests/corpus/curl/embedded_ansi_header.argv)
      corpus fixture with both `_plain.snap` and `_ansi.snap`, and a
      popover-side regression in
      [vetterd/src/tests/popover_attr.rs](vetterd/src/tests/popover_attr.rs)
      that confirms an argv-injected `\x1b[31m` no longer opens a
      spurious red span past the popover's SGR scanner
- [ ] `cargo-fuzz` target for `parsers::curl` over random argv;
      remove `expect("Value flag has value")` from
      `vetter-core/src/parsers/curl/state.rs` by encoding
      flag-has-value at the type level
- [ ] Wire-protocol fuzzing entry — was Phase 3 §6.3; promoted out
      of "Phase 3b polish" because peer-cred narrows but doesn't
      eliminate the local-attacker surface
- [x] `SignalKind::FollowRedirects` + `http: { no_redirects: true }`
      allowlist predicate — ThreatModel T10. Curl parser pushes a
      `FollowRedirects` (Warn) signal whenever `-L` / `--location`
      is present, the §9 generic catalogue lists the new bullet, and
      `HttpClause.no_redirects` defaults to `Some(true)` semantics so
      rules that genuinely need redirect-following trust must opt in
      via `no_redirects: false` in YAML — mirroring the
      `headers_allow` default-deny pattern.
      ([vetter-core/src/signals/mod.rs](vetter-core/src/signals/mod.rs),
      [vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/src/matcher/rule.rs](vetter-core/src/matcher/rule.rs),
      [vetter-core/src/matcher/decide.rs](vetter-core/src/matcher/decide.rs),
      [vetter-core/tests/corpus/curl/follow_redirects.argv](vetter-core/tests/corpus/curl/follow_redirects.argv))
- [x] Stdin body drift — ThreatModel T9. Closed by rejecting
      `-d @-` (and its `--data` / `--data-binary` / `--data-ascii` /
      `--data-urlencode` aliases) outright in the curl parser with
      `ParseError::StreamingUnsupported`, sister to the existing
      `-T -` / `-b @-` / `-w @-` / `-K -` rejections. Agents must
      materialise the body to a temp file and use `-d @file`, which
      round-trips through `Body::FromFile` + `FileRead` and stays
      auditable end-to-end. The unused `Body::FromStdin` variant,
      `ParsedCommand::stdin_digest` field, and `Sha256` newtype
      were scrubbed from `vetter-core` in the same change since
      no production code path can emit them anymore. The wire-v3
      stdin-forwarding work that option (a) once tracked is no
      longer required for curl; future parsers that genuinely need
      stdin payloads will revisit it.
      ([vetter-core/src/parsers/curl/state.rs](vetter-core/src/parsers/curl/state.rs),
      [vetter-core/src/parsers/types.rs](vetter-core/src/parsers/types.rs),
      [vetter-core/tests/corpus/curl/data_at_stdin.argv](vetter-core/tests/corpus/curl/data_at_stdin.argv))

### H3 — Supply chain  `[x] done`

Cheap, high-value, expected for a security tool.

- [x] CI: `cargo-deny check` (advisories, bans, sources, licenses)
      on every PR; treat advisory hits as build failures by default.
      `deny` job in [.github/workflows/ci.yml](.github/workflows/ci.yml)
      runs `EmbarkStudios/cargo-deny-action@v2` against the repo-root
      [deny.toml](deny.toml): advisories `yanked = "deny"` with no
      ignores, sources locked to crates.io (no git, no alternate
      registries), permissive-only license allowlist derived from the
      current `Cargo.lock` (LGPL/GPL/AGPL deliberately omitted),
      `multiple-versions = "warn"` and `wildcards = "deny"` on the
      bans block
- [x] CI: `cargo-audit` on every PR + a weekly scheduled run on
      `main` so we don't sit on a fresh advisory between PRs.
      `audit` job in [.github/workflows/ci.yml](.github/workflows/ci.yml)
      handles the per-PR coverage; the weekly cron lives in its own
      workflow at [.github/workflows/audit-weekly.yml](.github/workflows/audit-weekly.yml)
      (`07:17 UTC` Mondays + `workflow_dispatch`) so the scheduled
      run shows up as its own status check and can re-open RustSec
      advisory issues via `secrets.GITHUB_TOKEN`
- [x] Pin MSRV in CI matrix to match `rust-toolchain.toml` (1.95);
      add a "MSRV bump" PR template so toolchain changes are
      explicit, not silent. New `msrv` job in
      [.github/workflows/ci.yml](.github/workflows/ci.yml) pins
      `dtolnay/rust-toolchain@1.95` and runs the same
      `build --all-targets --all-features` + `test --all-features`
      block as the stable matrix on `ubuntu-latest`. The MSRV-bump
      checklist lives at
      [.github/PULL_REQUEST_TEMPLATE/msrv.md](.github/PULL_REQUEST_TEMPLATE/msrv.md)
      and is referenced via `?template=msrv.md` on the PR-create URL

### H4 — Public-release hygiene  `[ ] not started`

A security tool published on Homebrew needs a license, a
vulnerability-reporting policy, and a discoverable changelog. Without
these we can't reasonably ask anyone to trust the binary.

- [x] Add `LICENSE` file in repo root.
      `Cargo.toml` already declares `license = "MIT"`
      and the canonical license text is now in `LICENSE`.
- [ ] Add `SECURITY.md` with vulnerability reporting contact +
      disclosure policy (90-day default, GitHub Security Advisory
      preferred + email fallback). Link from README.
- [ ] Add `CHANGELOG.md` (Keep-a-Changelog style); seed with the
      v0.1.0 entry generated from git log + this TODO. Future
      releases: every PR that ships behaviour change updates the
      `[Unreleased]` section.
- [ ] Update `Cargo.toml` workspace.package metadata: add
      `description`, `homepage`, `documentation`, `keywords`,
      `categories`. Same metadata propagates into `vet --version`
      build info and into any future crates.io publish.
- [ ] README updates: drop "Pre-MVP" / "Not yet chosen" language,
      add a Security section pointing at SECURITY.md, add a
      License section pointing at the LICENSE files, link
      CHANGELOG, and update the "deliberately not there yet"
      paragraph against the new Backlog section below.
- [ ] Verify the signed bundle launches cleanly on a fresh user
      account: run the manual smoke procedure in
      [plans/MacOSApp.md](plans/MacOSApp.md) end-to-end after
      `xattr -d com.apple.quarantine target/Vetter.app` to
      simulate Gatekeeper's first-launch path

### H5 — Workspace & allowlist trust gates  `[ ] not started`

Closes ThreatModel T7. The project-scope allowlist discovery walks
up from `cwd` and loads any `.vet/allowlist.yaml` it finds before
hitting a `.git` boundary, with no per-repo opt-in. An untrusted
repo a coding agent checks out can ship its own rules
(`host: "*.attacker.example"`, `http: { no_body: false }`, etc.)
and the agent then auto-allows any call those rules cover — no
popover, no human in the loop. Same shape applies if the agent's
`cwd` ends up under an attacker-controlled path that carries a
`.vet/` directory without an intervening `.git`.

- [ ] Per-repo workspace-trust gate: on first discovery of a
      project-scope allowlist, refuse to load until the user has
      confirmed the repo out-of-band. Implementation shape: a
      `~/.vet/trusted-projects.yaml` keyed by `(project_root_path,
      allowlist_file_sha256)`; `vetterd` (and the `vet allow list`
      CLI) refuse entries whose `(path, hash)` pair isn't on the
      list, surfacing instead an "untrusted project" message with a
      `vet allow trust-project <path>` escape hatch. Invalidating
      on hash change means edits re-prompt — which is the right
      default for a repo where the rules change.
- [ ] File-ownership guard on allowlist / known-hosts loads: refuse
      any layered-discovery YAML whose `stat().st_uid` differs from
      `self_uid`, or whose containing directory is group-/world-
      writable. Cheap, independent of the workspace-trust list, and
      catches the "shared scratch dir" variant of T7 even when the
      user hasn't set up trusted-projects.
- [ ] Allowlist-file filesystem writes behind the popover: `vet
      allow add` currently edits `~/.vet/allowlist.yaml` directly,
      which a same-UID attacker can mimic (no approver gate on the
      filesystem path). Pair with H1's admin-socket write hardening
      so *every* rule persistence funnels through a single
      human-confirmed code path — either the popover picker or an
      in-process confirmation modal invoked by the CLI — and the
      raw YAML becomes read-only from the daemon's perspective at
      runtime.

---

## Hardening — post-launch follow-ups

Operational hygiene and audit fidelity. Not ship-blockers — none
expose a known privilege-escalation or render-spoofing path — but the
first batch of real users will surface them and we should land them
quickly after v0.1.

- [ ] Audit log rotation + size cap; explicit behaviour when audit
      dir is unwritable (warn loudly, don't silently drop)
- [ ] Hash the loaded ruleset; record digest in each audit row so
      decisions remain replayable after `allowlist.yaml` edits
- [ ] Forward stdin bytes for parsers that read stdin and re-inject
      on exec. Curl no longer needs this: H2 / ThreatModel T9 closed
      the gap by rejecting `-d @-` (and aliases) outright, so the
      daemon never sees a body sourced from a client-side pipe and
      the popover / audit log can no longer drift from what `vet`
      execs. This bullet stays open as a forward-looking concern
      for any future parser (`wget`, `httpie`) that genuinely
      needs to consume agent stdin: when one lands, the wire-v3
      stdin-digest forwarding (and exec-time re-injection) becomes
      the prerequisite for that parser to safely accept `-`-style
      stdin invocations.
- [ ] Decide symlink semantics for `Effect::FileWrite` /
      `Effect::FileRead` (canonicalise vs. reject vs.
      accept-and-document); add tests
- [ ] Document wire evolution rules (additive-only fields, unknown
      enum variants rejected) in `plans/Overview.md` §3

---

## Backlog — post-MVP

Tracked but not on the v0.1 critical path. Each is roughly
self-contained; pull from this list when v0.1 is out and stable.

### Phase 7 — Additional HTTP-client parsers

The allowlist model, matcher, risk analyzer, render layout, and
suggestion engine are all built around `Effect::HttpRequest` — URL
scheme/host/port/path, method, headers, body, TLS policy, redirects.
Only commands whose primary purpose is making HTTP requests map
cleanly onto this approval surface. Non-HTTP commands (`ssh`, `rm`,
`git push`, etc.) would need entirely different rule schemas,
signal heuristics, and approval UIs; they are out of scope.

Each parser below is a new file implementing `CommandParser` plus
snapshot fixtures. `wget` doubles as a generalisation check on the
Phase 1a interface — if it forces `Effect`/`ParsedCommand` changes,
fix those before the rest land.

- [ ] `wget` — closest sibling to curl; emits `HttpRequest` +
      `FileWrite` (default saves to disk). Handles `-O`, `--post-data`,
      `--header`, `--no-check-certificate`, `--max-redirect`, auth
      flags. Good first test of parser generality.
- [ ] `httpie` (`http` / `https` commands) — developer-oriented HTTP
      client with a distinctive `METHOD URL key=value` argv shape.
      Emits `HttpRequest` + optional `FileWrite` (`--output` /
      `--download`). Covers agents that prefer httpie over curl.

### Phase 8 — Other platforms (post-v0.2)

- [ ] Windows UI: WinRT toast + tray
- [ ] localhost web UI (`http://127.0.0.1:<port>`) as a uniform
      fallback for SSH / Chromebook / kiosk environments

### Deferred from Phase 4 / Phase 5

- [ ] CI release workflow on tag push: imports the Developer-ID
      cert + Notary `.p8` from repo secrets, runs
      `tools/release.sh`, attaches the zip to a GitHub Release,
      and opens a PR against `blevinstein/homebrew-vetter` with
      the bumped cask
- [ ] E2E happy-path + deny-path tests against the signed bundle
      (PR 1/2 cover them via `MockNotifier`; signed-app E2E waits
      on the notarisation pipeline being stable across releases)
- [ ] `vet allow suggest <id>` CLI prints the suggestions payload
      as YAML (developer-ergonomics shim — engine is reachable
      through the admin socket already)
- [ ] Allowlist suggestions over `Effect::FileWrite` /
      `Effect::FileRead` (engine returns empty for now; popover
      hides the button)

---

## Cross-cutting / open questions

Tracked from [plans/Overview.md](plans/Overview.md) §12:

- [x] Decide allowlist semantics for query strings (default off; opt-in
      `query:` matcher)
- [ ] Decide telemetry policy (default none; opt-in local-only metrics)
      — close this explicitly in `ThreatModel.md` rather than leaving
      it open
- [x] Distribution: Homebrew tap publishing the notarised `.app`
      (cask lives in the standalone
      [`blevinstein/homebrew-vetter`](https://github.com/blevinstein/homebrew-vetter)
      tap repo; release walkthrough in
      [plans/Release.md](plans/Release.md))

---

## How to update this file

When you complete a checkbox, change `[ ]` to `[x]` in the same commit
that lands the work. When you start a phase, change its top-level tag
to `[~] in progress`. New tasks discovered mid-phase get appended as
checkboxes within the relevant phase.
