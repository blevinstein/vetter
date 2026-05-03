# vetter

A local security gate that sits between an LLM coding agent and "dangerous"
CLI commands. The agent runs `vet curl …` instead of `curl …`; `vet` parses
the invocation, applies a layered allowlist, and either lets it through
silently, blocks it outright, or prompts you for a per-call decision via a
native macOS notification.

For the architecture, threat model, and rule semantics see
[plans/Overview.md](plans/Overview.md).

## Status

v0.1 release-candidate, with pre-release security hardening in
flight before the first public Homebrew tag. Phases 0–3 (workspace,
parser plugin contract, curl parser, allowlist evaluation, daemon +
IPC) are complete. Phase 4 (the all-in-Rust macOS approver) is
feature-complete for everyday use:

- `Vetter.app` menu-bar bundle, built from `vetterd` via
  [`tools/build-app.sh`](tools/build-app.sh).
- `UNUserNotificationCenter` notifications with **Approve** /
  **Reject** buttons; the daemon worker blocks until the user
  clicks one. The first prompt of a burst raises a banner;
  subsequent prompts coalesce into the menu-bar without spamming
  Notification Center.
- A menu-bar shield icon + pending-count badge, plus an `NSPopover`
  listing every pending request as a structured card — typed
  AppKit rows for the URL (with a host-trust pill), risk-signal
  chips, headers (with secret redaction), body, and per-effect
  file / process rows, plus a "Show raw" disclosure that keeps
  the canonical §8.5 plaintext layout reachable underneath.
  Tapping the body of a notification routes the user into the
  popover with the matching card scrolled into view; Approve /
  Reject from either surface dismisses any stale banner. See
  [plans/ApprovalUI.md](plans/ApprovalUI.md) for the catalogue.
- A separate `vet daemon list` / `vet daemon status` admin socket
  so the CLI can report real pending counts and queue contents
  without going through the prompt path.
- End-to-end coverage via a `MockNotifier`-driven test harness
  (which now also forwards the §8.5 rendered detail so future
  popover assertions don't need AppKit); real
  `UNUserNotificationCenter` integration is verified by the manual
  smoke test in [plans/MacOSApp.md](plans/MacOSApp.md).

A local Developer-ID signing + notarisation pipeline lands in
[`tools/release.sh`](tools/release.sh) +
[`plans/Release.md`](plans/Release.md). The Homebrew cask lives in
its own repo,
[`blevinstein/homebrew-vetter`](https://github.com/blevinstein/homebrew-vetter)
(a Homebrew tap has to be a standalone `homebrew-<name>` repo).

What's deliberately not there yet (all tracked in
[TODO.md](TODO.md)):

- **Pre-release ship-blockers** — Hardening §H1 (PID attestation,
  request read deadlines + worker cap, `argv[0]` inode resolution,
  `FD_CLOEXEC`), §H2 (ANSI / C0 sanitisation in the renderer,
  curl-parser fuzzing, wire-protocol fuzzing), §H3 (`cargo-deny`
  + `cargo-audit` in CI, MSRV pin), and §H4 (LICENSE files,
  SECURITY.md, CHANGELOG, README polish). These must close before
  the first public Homebrew tag.
- **Post-launch follow-ups** — audit log rotation, ruleset hashing
  in the audit row, stdin forwarding for `curl -d @-`, and
  symlink semantics for file effects. Won't expose a known
  privilege escalation; first batch of users will surface them
  and we'll land them quickly after v0.1.
- **Backlog (post-MVP)** — Linux/Windows UIs, additional command
  parsers (`wget`, `gh`, `aws`, `gcloud`, `ssh`, `rm`, `git
  push`), an automated CI release workflow on tag push, an E2E
  suite against the signed bundle, the banner-side `Allowlist…`
  notification action, the `vet allow suggest` CLI shim, and
  allowlist suggestions over file effects.

## Trying it locally

macOS only for now (Phase 4 ships the macOS UI; non-macOS UIs are
Phase 6 territory). Walkthrough including first-run permission
prompts, gotchas, and the audit-log check is in
[plans/MacOSApp.md](plans/MacOSApp.md). Short version (build from
source):

```sh
tools/build-app.sh --release          # builds vetterd + vet into the bundle
open target/Vetter.app                 # menu-bar app, no dock icon
export PATH="$PWD/target/Vetter.app/Contents/MacOS:$PATH"
vet curl https://prompt-test.example/  # banner with Approve/Reject;
                                       # click the icon for the popover
```

A signed + notarised bundle suitable for distribution is built by
[`tools/release.sh`](tools/release.sh) — see
[plans/Release.md](plans/Release.md) for the Apple-side prereqs.
Once the first release is published the install path will simplify
to:

```sh
brew tap blevinstein/vetter
brew install --cask vetter
open /Applications/Vetter.app
```

On macOS the daemon **expects** to run inside the `Vetter.app`
bundle: that's where Launch Services applies `LSEnvironment`,
`UNUserNotificationCenter` recognises the code-signed identity, and
the menu-bar status item registers correctly. Starting the daemon
any other way (`cargo run --bin vetterd`, exec'ing
`target/release/vetterd` directly, or `vet daemon start` on a host
without a built `Vetter.app`) would yield a half-running daemon
that hangs every prompt-class request, so we fail closed: the
default `VETTERD_NOTIFIER=mac` notifier refuses to install if the
executable is not under `Vetter.app/Contents/MacOS/`, and the
process exits with code 78.

For non-macOS targets, headless CI, or dev workflows that
intentionally skip the UI, set `VETTERD_NOTIFIER=noop` (every
prompt-class request will then hang until the daemon is killed —
useful for lifecycle smoke checks, not for actual use).

## Workspace layout

Rust workspace, `cargo` for everything:

- [`vetter-core/`](vetter-core/) — shared library: `ParsedCommand`,
  `Effect`, parser registry, renderer, risk analyzer, wire types.
- [`vet/`](vet/) — the CLI users (and agents) invoke.
- [`vetterd/`](vetterd/) — the long-running per-user daemon; on macOS
  this binary is also the body of `Vetter.app`.

Build / fmt / clippy / test:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test  --workspace --all-features
```

## Documentation index

| If you want to… | Read |
|---|---|
| Understand the architecture, types, and rule model | [plans/Overview.md](plans/Overview.md) |
| See what's done / next / blocked | [TODO.md](TODO.md) |
| Test plan and per-component coverage targets | [plans/TestingPlan.md](plans/TestingPlan.md) |
| Threat model + hardening backlog | [plans/ThreatModel.md](plans/ThreatModel.md) |
| Build / run / smoke-test the macOS app | [plans/MacOSApp.md](plans/MacOSApp.md) |
| Sign, notarise, publish to the Homebrew tap | [plans/Release.md](plans/Release.md) |
| Work on the repo as an AI agent (or a human) | [AGENTS.md](AGENTS.md) |

## License

Licensed under MIT (see
[`Cargo.toml`](Cargo.toml) `workspace.package.license`); choose
whichever fits your downstream use. The canonical license texts
will land as `LICENSE-MIT` in the repo root as part of Hardening
§H4 before the first public Homebrew tag.

## Security

A `SECURITY.md` with a vulnerability-reporting contact and
disclosure policy lands as part of Hardening §H4 before the first
public Homebrew tag. Until then please open a private GitHub
Security Advisory or email the repo owner directly. The active
threat model and the unmitigated attacks we plan to fix are
catalogued in [plans/ThreatModel.md](plans/ThreatModel.md).
