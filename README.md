# vetter

A local security gate between your AI coding agent and dangerous shell commands.

When an LLM agent wants to run `curl`, it calls `vet curl …` instead. Vetter
parses the invocation, checks it against your allowlist, and either lets it
through silently, blocks it outright, or pops up a native desktop notification
asking you to **Approve** or **Reject** — before the command runs.

## How it works

1. **Install** vetter and tell your agent to prefix `curl` (and other commands)
   with `vet`.
2. **Set your allowlist** — rules covering hosts, paths, methods, and risk
   signals that you always want to allow through.
3. **Stay in control** — anything outside the allowlist reaches you as a
   native notification with full context: URL, headers (secrets redacted),
   body, and a summary of what the command would affect.

## Status

**v0.1 release candidate** — macOS support is feature-complete and in use
daily. Ubuntu is planned for v0.2. A pre-release security hardening pass is in
flight before the first public Homebrew tag; see [TODO.md](TODO.md) for the
full checklist.

What works today on macOS:

- Menu-bar app (`Vetter.app`) with a shield icon and pending-count badge
- Native notifications with **Approve** / **Reject** buttons
- A popover listing every pending request as a structured card, with risk
  signals highlighted and secret headers redacted
- Layered allowlist (global → project → per-call)
- Audit log of every decision

## Installation

### macOS (Homebrew coming soon — build from source today)

```sh
tools/build-app.sh --release
open target/Vetter.app
export PATH="$PWD/target/Vetter.app/Contents/MacOS:$PATH"
```

After the first launch, click the menu-bar shield and tick **Start at login**
so Vetter restarts automatically after every reboot.

Once the first release is published the install path will simplify to:

```sh
brew tap blevinstein/vetter
brew install --cask vetter
open /Applications/Vetter.app
```

### Ubuntu (v0.2, planned)

```sh
sudo add-apt-repository ppa:blevinstein/vetter
sudo apt-get update && sudo apt-get install vetter
# log out and back in so the tray app auto-starts
```

## Quick start

```sh
vet curl https://api.example.com/data    # triggers an Approve/Reject notification
vet curl https://trusted-host.example/  # passes silently if in your allowlist
```

A walkthrough of first-run permission prompts, allowlist setup, and the audit
log is in [plans/MacOSApp.md](plans/MacOSApp.md).

## Roadmap

| Milestone | Status |
|---|---|
| v0.1 — macOS, `curl` parser, allowlist, notification UI | Release candidate |
| v0.2 — Ubuntu (D-Bus notifications, GTK4 popover, `.deb` / PPA) | Design |
| Backlog — Windows, `wget` / `gh` / `aws` / `gcloud` / `ssh` / `rm` / `git push` parsers, web UI | Planned |

## License

MIT — see [`Cargo.toml`](Cargo.toml). Full `LICENSE-MIT` file lands before the
first public Homebrew tag.

## Security

A `SECURITY.md` with a vulnerability-reporting contact and disclosure policy
lands before the first public Homebrew tag. Until then, please open a private
GitHub Security Advisory or email the repo owner directly. The threat model is
catalogued in [plans/ThreatModel.md](plans/ThreatModel.md).
