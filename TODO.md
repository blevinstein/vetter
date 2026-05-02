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

## Phase 1b — Curl parser  `[ ] not started`

Roadmap: [plans/Overview.md](plans/Overview.md) §8.4, §11. Test plan:
[plans/TestingPlan.md](plans/TestingPlan.md) §3.

- [ ] `parsers/curl/` module with full argv parser covering the curl
      flag set (`flags.rs`)
- [ ] Emits `Effect::HttpRequest` (+ optional `FileWrite` for
      `-o`/`-O`/`-J`, `FileRead` for `-T <file>` and `-d @file`)
- [ ] Curl-specific `RiskSignal`s pushed during parse: `--insecure` /
      `-k`, `--cacert`, `--resolve`, `--unix-socket`
- [ ] Refuses streaming bodies with `ParseError::StreamingUnsupported`
      (`-T -`, chunked transfer, `-d @-` >1 MiB)
- [ ] Snapshot fixture corpus in `vetter-core/tests/corpus/curl/` per
      `TestingPlan.md` §3.1 (~25 fixtures incl. negative cases)
- [ ] `vet --explain curl …` runs end-to-end against the Phase 1a
      renderer + analyzer with no daemon, no policy
- [ ] CLI: TTY / `NO_COLOR` / `CLICOLOR_FORCE` detection chooses
      `PlainWriter` vs `AnsiWriter`
- [ ] `vet doctor` reports `parsers registered . 1` (curl)

## Phase 2 — Allowlist evaluation  `[ ] not started`

Roadmap: [plans/Overview.md](plans/Overview.md) §5. Test plan:
[plans/TestingPlan.md](plans/TestingPlan.md) §2.5, §2.6.

- [ ] YAML schema + loader for user scope (`~/.config/vet/allowlist.yaml`)
      and project scope (`<repo>/.vet/allowlist.yaml`); walks up from cwd
- [ ] Rule matcher in `vetter-core::matcher` against `effects`; layered
      precedence (denylist > session > project > user > built-in)
- [ ] `headers_allow` default-deny semantics; explicit `*` opt-in
- [ ] Path/host glob matching; URL normalisation before match
- [ ] `vet --explain curl …` reports allow / deny / prompt with
      matched rule id
- [ ] `vet allow add` / `rm` / `list` subcommands fully wired
- [ ] Property tests over rule matching (TestingPlan §2.5)
- [ ] Negative tests: host-suffix confusion, path traversal, denylist
      overrides allow at every layer

## Phase 3 — Daemon + IPC  `[ ] not started`

Roadmap: [plans/Overview.md](plans/Overview.md) §3, §11. Test plan:
[plans/TestingPlan.md](plans/TestingPlan.md) §5, §6.

- [ ] `vetterd` socket listener at `$TMPDIR/vetter.sock`; one
      request → one decision; length-prefixed JSON
- [ ] `VetRequest` / `VetDecision` wire types in `vetter-core::wire`
      with `v` field for version negotiation
- [ ] Pending-request queue; concurrent clients get independent
      decisions
- [ ] `vet` becomes a thin client; fails closed (exit non-zero) when
      the socket is missing — no TTY fallback
- [ ] Audit log at `~/Library/Logs/vetter/audit.log` (JSON lines),
      flushed before decision returns
- [ ] Stub UI for prompt-class decisions: auto-deny with reason
      `"no UI yet"`
- [ ] `vet daemon start|stop|status` controls supervise `vetterd`
- [ ] Atomic allowlist writes (temp + rename); §5.3 crash test
- [ ] §4.9 fail-closed test (no exec on deny / missing socket)
- [ ] §6.3 protocol fuzzing entry

## Phase 4 — macOS approver UI  `[ ] not started`  (MVP milestone)

Roadmap: [plans/Overview.md](plans/Overview.md) §7.

- [ ] Swift menu-bar `.app` bundle, `LSUIElement`, no dock icon
- [ ] `UNUserNotificationCenter` integration: Approve /
      Allowlist… / Reject actions
- [ ] Popover listing pending requests with §8.5 rendered summary
      and detail view
- [ ] Notification coalescing into menu-bar after the first banner
- [ ] Code signing + notarisation pipeline; Homebrew tap
- [ ] Full §7 cross-cutting security property suite passes
- [ ] E2E happy-path + deny-path tests

## Phase 5 — Pattern suggestions  `[ ] not started`

Roadmap: [plans/Overview.md](plans/Overview.md) §5 ("Pattern
suggestions"), §11.

- [ ] Generalisation engine: exact → path-glob → method+host
- [ ] "Allowlist…" action opens picker UI from Phase 4
- [ ] Chosen rule appended to user or project scope per selection
- [ ] Unit tests over each generalisation tier

## Phase 6 — Other platforms  `[ ] not started`  (post-MVP)

- [ ] Linux UI: libnotify + AppIndicator
- [ ] Windows UI: WinRT toast + tray
- [ ] localhost web UI (uniform fallback)

## Phase 7+ — Additional command parsers  `[ ] not started`

Each is a new file implementing `CommandParser` plus snapshot fixtures.
The first one doubles as a generalisation check on the Phase 1a
interface — if it forces `Effect`/`ParsedCommand` changes, fix those
before the rest land.

- [ ] `wget`  (Phase 1a interface check; do this first)
- [ ] `gh`
- [ ] `aws`
- [ ] `gcloud`
- [ ] `ssh` / `scp`
- [ ] `rm`
- [ ] `git push` / `git remote`

---

## Cross-cutting / open questions

Tracked from [plans/Overview.md](plans/Overview.md) §12:

- [ ] Decide allowlist semantics for query strings (default off; opt-in
      `query:` matcher)
- [ ] Decide telemetry policy (default none; opt-in local-only metrics)
- [ ] Distribution: Homebrew tap publishing the notarised `.app`

---

## How to update this file

When you complete a checkbox, change `[ ]` to `[x]` in the same commit
that lands the work. When you start a phase, change its top-level tag
to `[~] in progress`. New tasks discovered mid-phase get appended as
checkboxes within the relevant phase.
