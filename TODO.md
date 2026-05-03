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
- [ ] `Allowlist…` action on the notification banner itself
      (Phase 5 added per-card popover buttons, which cover the
      user need; the notification-button variant is convenience
      only — deferred to Backlog)
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
- [ ] CI release workflow on tag push — **deferred to Backlog**.
      Local `tools/release.sh` is sufficient for v0.1; automating
      from CI is convenience.
- [ ] Full §7 cross-cutting security property suite passes —
      **superseded by Hardening §H1–H2 below**, which track the
      §7 properties as concrete owning items.
- [ ] E2E happy-path + deny-path tests against the signed bundle —
      **deferred to Backlog**. PR 1/2 cover the logic via
      `MockNotifier`; a signed-app E2E job waits on the
      notarisation pipeline being stable across releases.
- [x] `vet daemon list` — CLI command to list pending approvals over
      a new admin socket (`vetter-admin.sock`); `vet daemon status`
      now shows real pending count
      ([vet/src/daemon.rs](vet/src/daemon.rs),
      [vetterd/src/lib.rs](vetterd/src/lib.rs),
      [vetter-core/src/wire/mod.rs](vetter-core/src/wire/mod.rs),
      [vetterd/tests/admin_ipc.rs](vetterd/tests/admin_ipc.rs))

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
- [ ] `vet allow suggest <id>` CLI prints the suggestions payload
      as YAML — **deferred to Backlog**. The engine is reachable
      through the admin socket already; this is a developer-
      ergonomics shim.
- [ ] Allowlist suggestions over `Effect::FileWrite` /
      `Effect::FileRead` — **deferred to Backlog**. Engine returns
      empty for now; popover hides the button.

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
- [ ] `FD_CLOEXEC` on daemon + client sockets and the audit fd; test
      that the exec'd child inherits only 0/1/2 (no leaked daemon fd
      survives the `execvp` into the wrapped command)
- [ ] Verify socket parent dir owner + mode before `vet` connects;
      refuse on mismatch. (Daemon enforces `0700` at bind; the
      client-side check is the residual T1 gap — peer-cred largely
      neutralises it but the check is cheap.)
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
- [ ] Explicit `0600` mode on audit-log, allowlist, and known-hosts
      writes — ThreatModel T8. `vetterd/src/audit.rs::AuditLog::open`
      opens with `OpenOptions::new().create(true).append(true)` (no
      `.mode(0o600)`), and `vetter_core/src/matcher/loader.rs::
      write_file` persists via `tempfile::NamedTempFile` without a
      `permissions()` override. Under the default macOS/Linux umask
      (`022`) both land as `0644` — other local UIDs can read the
      audit log (which carries verbatim argv, i.e. routinely carries
      secrets) and the user's allowlist / known-hosts YAML (trust-
      surface reconnaissance). Fix: `OpenOptions::mode(0o600)` on
      `AuditLog::open`, `tempfile::Builder::new().permissions(
      Permissions::from_mode(0o600))` on the matcher / known-hosts
      write path, and chmod `~/.vet/` and `~/Library/Logs/vetter/`
      to `0700` at create time. Extend `vet doctor` to flag any of
      these files whose on-disk mode is wider than `0600` (or whose
      parent dir is wider than `0700`).
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

- [ ] Sanitise ANSI / C0 control bytes in renderer output (header
      values, URLs, paths, argv echo) before any TTY or popover
      write. Corpus test with embedded ANSI cursor moves, RTLO
      (U+202E), zero-width chars, and bare CR / BS so a malicious
      argv can't repaint the screen or hide a path segment.
- [ ] `cargo-fuzz` target for `parsers::curl` over random argv;
      remove `expect("Value flag has value")` from
      `vetter-core/src/parsers/curl/state.rs` by encoding
      flag-has-value at the type level
- [ ] Wire-protocol fuzzing entry — was Phase 3 §6.3; promoted out
      of "Phase 3b polish" because peer-cred narrows but doesn't
      eliminate the local-attacker surface
- [ ] `SignalKind::FollowRedirects` + `http: { no_redirects: true }`
      allowlist predicate — ThreatModel T10. Today
      `HttpRequest.follow_redirects` is tracked on the effect but
      produces no signal and no predicate, so an allowlisted host
      that open-redirects (or is compromised) can bounce the
      request to any origin while the auto-allow fires on the
      initial URL. Fix: push a `FollowRedirects` signal from the
      curl parser whenever `-L` / `--location` is present, extend
      the generic analyzer / signals §9 catalogue to mention it,
      and add a `no_redirects: true` (default) / explicit opt-in
      predicate on the `http:` rule clause so rules that genuinely
      need redirect-following trust have to say so in YAML.
- [ ] Stdin body drift — ThreatModel T9, promoted from the
      post-launch follow-ups below because the popover and audit
      log materially misrepresent what `vet` execs when the agent
      pipes bytes into `curl -d @-`. Either (a) forward
      `stdin_digest` + `stdin_len` on the v3 wire frame (client
      hashes before the daemon round-trip and re-injects on
      `execvp`), or (b) emit a `StdinBody` signal whenever
      `Body::FromStdin` appears so the auto-allow path closes for
      stdin-bearing calls until the human has confirmed the
      payload shape. Either close is fine; (b) is a one-line
      change that immediately removes the "approve 0 B, exfil N MB"
      gap even before the stdin forwarding lands.

### H3 — Supply chain  `[ ] not started`

Cheap, high-value, expected for a security tool.

- [ ] CI: `cargo-deny check` (advisories, bans, sources, licenses)
      on every PR; treat advisory hits as build failures by default
- [ ] CI: `cargo-audit` on every PR + a daily scheduled run on
      `main` so we don't sit on a fresh advisory between PRs
- [ ] Pin MSRV in CI matrix to match `rust-toolchain.toml` (1.95);
      add a "MSRV bump" PR template so toolchain changes are
      explicit, not silent

### H4 — Public-release hygiene  `[ ] not started`

A security tool published on Homebrew needs a license, a
vulnerability-reporting policy, and a discoverable changelog. Without
these we can't reasonably ask anyone to trust the binary.

- [ ] Add `LICENSE-MIT` file in repo root.
      `Cargo.toml` already declares `license = "MIT"`
      but the canonical license text is missing from the repo.
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
- [ ] Forward stdin bytes for parsers that read stdin (curl `-d @-`)
      and re-inject on exec; today daemon parses with empty stdin
      so `-d @-` round-trips as `Body::FromStdin{len: 0}`. Audit
      logs and policy decisions for those calls reflect the empty
      body, not the bytes curl actually sees. The **approval-
      surface drift** slice of this has been elevated to H2 as
      ThreatModel T9 (popover / audit misrepresent what `vet`
      execs when the pipe carries secrets); what remains here is
      the full "forward + re-inject" implementation once the
      interim `StdinBody` signal / wire-v3 digest work lands.
- [ ] Decide symlink semantics for `Effect::FileWrite` /
      `Effect::FileRead` (canonicalise vs. reject vs.
      accept-and-document); add tests
- [ ] Document wire evolution rules (additive-only fields, unknown
      enum variants rejected) in `plans/Overview.md` §3

---

## Backlog — post-MVP

Tracked but not on the v0.1 critical path. Each is roughly
self-contained; pull from this list when v0.1 is out and stable.

### Phase 6 — Other platforms

- [ ] Linux UI: libnotify + AppIndicator
- [ ] Windows UI: WinRT toast + tray
- [ ] localhost web UI (uniform fallback)

### Phase 7+ — Additional command parsers

Each is a new file implementing `CommandParser` plus snapshot
fixtures. The first one doubles as a generalisation check on the
Phase 1a interface — if it forces `Effect`/`ParsedCommand` changes,
fix those before the rest land.

- [ ] `wget`  (Phase 1a interface check; do this first)
- [ ] `gh`
- [ ] `aws`
- [ ] `gcloud`
- [ ] `ssh` / `scp`
- [ ] `rm`
- [ ] `git push` / `git remote`

### Deferred from Phase 4 / Phase 5

- [ ] CI release workflow on tag push: imports the Developer-ID
      cert + Notary `.p8` from repo secrets, runs
      `tools/release.sh`, attaches the zip to a GitHub Release,
      and opens a PR against `blevinstein/homebrew-vetter` with
      the bumped cask
- [ ] E2E happy-path + deny-path tests against the signed bundle
      (PR 1/2 cover them via `MockNotifier`; signed-app E2E waits
      on the notarisation pipeline being stable across releases)
- [ ] `Allowlist…` button on the notification banner itself
      (Phase 5's per-card popover buttons cover the user need;
      adding the action to the banner is convenience)
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
