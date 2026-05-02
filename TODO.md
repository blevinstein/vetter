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

## Hardening — cross-cutting (pre-MVP gate)

Roadmap source: tech-debt audit, 2026-05. None of these block a phase
by themselves, but Phase 4 (UI) shouldn't ship without them — they are
the difference between "demo-quality daemon" and "I'd run this on my
machine". Per-threat detail in
[plans/ThreatModel.md](plans/ThreatModel.md).

- [x] Write `plans/ThreatModel.md` (assets, attackers, in-scope /
      out-of-scope, residual risks) — landed in PR #4
- [x] Peer credential check on `vetterd` accept (`SO_PEERCRED` /
      `getpeereid`); reject connections whose uid doesn't match the
      daemon's — landed in PR #5 (T1 partial)
- [x] Drop `parsed` from `VetRequest`; daemon re-parses argv before
      matcher runs (T2). Wire bumped to v2; client / daemon /
      audit-log all re-derive command from `argv[0]`
- [ ] Verify socket parent dir owner + mode before `vet` connects;
      refuse on mismatch. (Daemon enforces `0700` at bind; the
      client-side check is the residual gap — peer-cred largely
      neutralises it.)
- [ ] PID attestation on connect (`vetterd` flocks the pidfile;
      `vet` asserts the locking PID equals the peer PID) — closes
      the rest of T1
- [ ] Read deadline on `vetterd`'s request frame + cap inflight
      workers (T3)
- [ ] Resolve `argv[0]` to a real path / inode before parser dispatch
      so `ln /bin/bash /tmp/curl && vet /tmp/curl …` can't route to
      the wrong parser (T4)
- [ ] `FD_CLOEXEC` on daemon + client sockets and the audit fd; test
      that the exec'd child inherits only 0/1/2
- [ ] Sanitise ANSI / C0 control bytes in renderer output (header
      values, URLs, paths, argv echo) before any TTY write — argv is
      attacker-controlled and the user is being asked to trust what
      they see
- [ ] `cargo-fuzz` target for `parsers::curl` over random argv;
      remove `expect("Value flag has value")` from
      `parsers/curl/state.rs` by encoding flag-has-value at the type
      level
- [ ] Decide symlink semantics for `Effect::FileWrite` /
      `Effect::FileRead` (canonicalise vs. reject vs.
      accept-and-document); add tests
- [ ] Audit log rotation + size cap; explicit behaviour when audit
      dir is unwritable (warn loudly, don't silently drop)
- [x] `serde(deny_unknown_fields)` on `VetRequest` / `VetDecision`
      (already in place; new wire-format test enforces that legacy
      `parsed` payloads are rejected)
- [ ] Document wire evolution rules (additive-only fields, unknown
      enum variants rejected) in `plans/Overview.md` §3
- [ ] Hash the loaded ruleset; record digest in each audit row so
      decisions remain replayable after `allowlist.yaml` edits
- [ ] Expand `vet doctor` checks: socket perms, audit dir writable,
      allowlist parses, daemon reachable
- [ ] CI: `cargo-deny check` (advisories, bans, sources, licenses)
      and `cargo-audit`
- [ ] Forward stdin bytes for parsers that read stdin (curl `-d @-`)
      and re-inject on exec; today daemon parses with empty stdin so
      `-d @-` round-trips as `Body::FromStdin{len: 0}`. Audit logs
      and policy decisions for those calls reflect the empty body,
      not the bytes curl actually sees. Tracked here because it
      surfaced while landing T2.

Promoted from elsewhere because the audit raised their priority:

- [ ] Wire protocol fuzzing — was Phase 3 §6.3; promote out of
      "Phase 3b polish"

---

## Cross-cutting / open questions

Tracked from [plans/Overview.md](plans/Overview.md) §12:

- [x] Decide allowlist semantics for query strings (default off; opt-in
      `query:` matcher)
- [ ] Decide telemetry policy (default none; opt-in local-only metrics)
      — close this explicitly in `ThreatModel.md` rather than leaving
      it open
- [ ] Distribution: Homebrew tap publishing the notarised `.app`

---

## How to update this file

When you complete a checkbox, change `[ ]` to `[x]` in the same commit
that lands the work. When you start a phase, change its top-level tag
to `[~] in progress`. New tasks discovered mid-phase get appended as
checkboxes within the relevant phase.
