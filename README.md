# vetter

A local security gate between your AI coding agent and dangerous shell commands.

When an LLM agent wants to run `curl`, it calls `vet curl …` instead. Vetter
parses the invocation, checks it against your allowlist, and either lets it
through silently, blocks it outright, or pops up a native desktop notification
asking you to **Approve** or **Reject** — before the command runs.

## How it works

**Step 1 — Install vetter** and start it as a menu-bar app.

**Step 2 — Allow `vet` in your agent harness.** In Claude Code, Cursor, or
whichever tool you use, add `vet` to the list of commands the agent can run
without asking you first. This is the key insight: you're not allowing `curl`
directly (which would be unsafe), you're allowing `vet curl …`, and vetter
enforces the real policy. The agent can't bypass vetter by tweaking flags.

**Step 3 — Set your allowlist.** Rules covering hosts, paths, HTTP methods,
and risk signals that should always pass through silently. Everything else
triggers a notification.

**Step 4 — Stay in control.** Anything outside the allowlist reaches you as a
native notification with full context: URL, headers (secrets redacted), body,
and a summary of what the command would affect. Approve or reject with one
click.

## Status

**v0.1** — macOS support is feature-complete and in use daily. Ubuntu is
planned for v0.2.

What works today on macOS:

- Menu-bar app (`Vetter.app`) with a shield icon and pending-count badge
- Native notifications with **Approve** / **Reject** buttons
- A popover listing every pending request as a structured card, with risk
  signals highlighted and secret headers redacted
- Layered allowlist (global → project → per-call)
- Audit log of every decision

## Installation

### macOS

```sh
brew tap blevinstein/vetter
brew install --cask vetter
open /Applications/Vetter.app
```

After the first launch, click the menu-bar shield and tick **Start at login**
so Vetter restarts automatically after every reboot.

**Building from source** (for development or if you prefer not to use the tap):

```sh
tools/build-app.sh --release
open target/Vetter.app
export PATH="$PWD/target/Vetter.app/Contents/MacOS:$PATH"
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
| v0.1 — macOS, `curl` parser, allowlist, notification UI | Released |
| v0.2 — Ubuntu (D-Bus notifications, GTK4 popover, `.deb` / PPA) | Design |
| Backlog — Windows, additional parsers, web UI | Planned |

## License

[MIT](LICENSE) — Copyright (c) 2024 Brian Levinstein.

## Security

Please open a private GitHub Security Advisory or email the repo owner
directly to report vulnerabilities. The threat model is catalogued in
[plans/ThreatModel.md](plans/ThreatModel.md).
