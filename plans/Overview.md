# vetter — Overview

`vetter` is a local security gate that sits between an LLM coding agent and
"dangerous" CLI commands. The CLI is named `vet`. Agents are configured to
allowlist `vet *` (instead of `curl *`, `wget *`, `gh *`, …); `vet` then runs
its own, more sophisticated approval policy — backed by a separate-channel UI
operated by the human approver, not by the agent.

The MVP wraps `curl`. The architecture is built so additional command-aware
parsers (wget, gh, aws, gcloud, ssh, rm, …) can be added later.

---

## 1. Problem

Modern agent harnesses (Cursor, Claude Code, etc.) let users allowlist
commands so the agent stops asking before each invocation. The available
granularity is bad:

- `curl *` — far too permissive (the agent can hit any URL, exfiltrate, etc.).
- per-call approvals — too noisy; users develop "approve fatigue" and rubber-
  stamp things.
- per-host allowlists in the harness — coarse, host-only, not method/path/body
  aware, and the prompt UI buries detail behind a wall of flags.

`vet` is the policy layer the harness doesn't have. It:

1. **Parses** the wrapped command with a command-specific parser.
2. **Renders** the parsed call in a consistent, color-coded layout so the
   human can scan it in <2 seconds.
3. **Evaluates** the call against a layered allowlist.
4. If a decision can't be made automatically, **routes** the request to a
   separate UI channel for the human (so the agent can't auto-approve its
   own request).
5. On approval, optionally **generalises** the rule and offers it as an
   allowlist pattern.

---

## 2. End-to-end user flow

```
agent harness        │  vet CLI            │  vetter daemon       │  approver UI
─────────────────────┼─────────────────────┼──────────────────────┼──────────────
 vet curl -X POST … ─▶ parse curl args
                       render summary
                       layered lookup ─────▶ allowlist eval
                                               ├─ allow  ───┐
                                               ├─ deny   ───┤
                                               └─ prompt ───┴─▶ notification /
                                                                 menu-bar / TUI
                                                                  ▲       │
                                                                  └ human ┘
                                                                          │
                       ◀───────── decision ─────────────────────────────  ┘
   (exit 0 + exec) ◀── exec curl on allow
   (exit non-0)    ◀── refuse on deny
```

Critical property: **the agent only sees the final exit code / passthrough
output.** The approval UI lives in the human's session, not the agent's
terminal, so the agent cannot click its own button.

---

## 3. Architecture

Two processes:

### `vet` (CLI, short-lived)
- Parses argv via a command-aware parser plugin.
- Renders the request locally for the user's reference (also useful when run
  outside an agent context).
- Connects to `vetterd` over a Unix domain socket
  (`$XDG_RUNTIME_DIR/vetter.sock`, falling back to `~/.vet/vetter.sock`).
- Sends a `VetRequest`, blocks on a `VetDecision`.
- On `allow`, `execvp`s the underlying command, inheriting stdio so behavior
  is indistinguishable from running `curl` directly.
- On `deny`, prints reason on stderr and exits with code `77` (sysexits
  `EX_NOPERM`).
- If the daemon socket is missing, falls back to in-process evaluation +
  blocking TTY prompt. This keeps `vet` usable on CI and SSH sessions, and
  makes the daemon strictly an enhancement.

### `vetterd` (daemon, long-running, per-user)
- Owns the allowlist files, the in-memory policy, the pending-request queue,
  and the platform-specific approval UI.
- Listens on the Unix socket; one request → one decision.
- Persists allowlist mutations atomically.
- Surfaces pending approvals via:
  - macOS: bundled menu-bar app with click-to-review window (notifications
    too, see §7).
  - Linux: libnotify with actions, fallback to `vetterd ui` TUI.
  - Windows: WinRT toast w/ buttons, fallback TUI.
- Optional read-only HTTP endpoint on `127.0.0.1` for richer review (browser
  view of pending request with diffs, decoded body, etc.).

### Wire protocol
JSON over the Unix socket, length-prefixed. Versioned. Sketch:

```jsonc
// VetRequest
{
  "v": 1,
  "id": "01HX…",            // ULID, used for log correlation
  "cwd": "/Users/me/proj",
  "agent_hint": "claude-code", // best-effort, from env (CLAUDE_CODE=1, etc.)
  "command": "curl",
  "argv": ["-X", "POST", "https://…", "-d", "@-"],
  "stdin_digest": "sha256:…", // if stdin is being piped, we hash it
  "parsed": { /* normalised command-specific struct, see §8 */ }
}

// VetDecision
{
  "v": 1,
  "id": "01HX…",
  "decision": "allow" | "deny" | "allow_once",
  "reason": "matched rule github-readonly",
  "rule_added": null | { /* allowlist rule, if approver chose 'allowlist' */ }
}
```

---

## 4. CLI surface

```
vet <command> [args…]            # primary: parse, evaluate, run
vet --explain <command> [args…]  # show parse + policy decision, do not run
vet --dry-run <command> [args…]  # always treat as prompt; never exec
vet allow add <pattern>          # imperative allowlist mgmt
vet allow rm <id>
vet allow list [--scope project|user]
vet doctor                       # check daemon, socket, parsers, signing
vet daemon start|stop|status     # supervise vetterd
```

Conventions:
- Exit codes: `0` (allowed + ran successfully), `77` (denied), `78` (config
  error), otherwise the wrapped command's exit code.
- Output: structured render goes to stderr by default so stdout passthrough
  is preserved for the wrapped command. `--quiet` suppresses the render.
- Color: respects `NO_COLOR`, `CLICOLOR_FORCE`, and TTY detection.

---

## 5. Allowlist model

### Storage layers (later wins)

1. **Built-in baseline** — bundled with the binary, very small. Examples:
   `GET https://registry.npmjs.org/**`, `GET https://pypi.org/simple/**`.
   Disabled by default; user opts in.
2. **User scope** — `~/.config/vet/allowlist.yaml`. Personal, never commited.
3. **Project scope** — `<repo>/.vet/allowlist.yaml`. Discovered by walking
   up from `cwd` to a directory containing this file or `.git`. Designed to
   be checked in so a team shares vetted patterns.
4. **Session scope** — in-memory only, populated by `allow_once` decisions
   from the UI. Discarded on daemon restart.
5. **Denylist** — same shape, takes precedence over allows at every layer.

### Rule shape (curl example)

```yaml
rules:
  - id: github-readonly
    command: curl
    when:
      method: [GET, HEAD]
      url:
        scheme: https
        host: api.github.com
        path: "/repos/**"
      headers_allow: [Accept, User-Agent, Authorization]
      no_body: true
    note: "Read-only GitHub API calls"
    created_by: alice
    created_at: 2026-04-30T18:11Z

  - id: localhost-dev
    command: curl
    when:
      url:
        host: ["localhost", "127.0.0.1", "::1"]
        port: [3000, 8000, 8080]
    note: "Local dev servers"
```

A rule matches iff every populated `when` field matches. Anything not
listed in `headers_allow` (or set to `*`) is a mismatch — i.e. **default-
deny on header surface**, because Authorization, Cookie, X-Api-Key, etc.
are exactly what we want to scrutinise.

### Pattern suggestions

When a request reaches the human and they approve, the daemon proposes
1–3 generalised rules ordered from tightest to loosest:

1. Exact match (host + method + full path).
2. Path-glob generalisation (`/repos/foo/bar` → `/repos/*/*`).
3. Method-and-host only.

Human picks one (or none) via the UI; chosen rule is appended to user
or project scope per their selection.

---

## 6. Threat model & non-goals

**Protects against**: an aligned-but-fallible LLM agent issuing requests
the user wouldn't authorise if asked individually — wrong host, wrong
method, sensitive credentials in headers, surprise POSTs, etc.

**Does not protect against** an adversarial agent that bypasses `vet`
entirely. If the harness allowlists `python *` or `bash *`, the agent can
trivially make HTTP requests without going through `vet`. The trust
boundary is: **the harness allowlists `vet *` and denies network-capable
commands directly.** We will publish a recommended harness config
alongside the project.

**Other non-goals**:
- We are not a full sandbox. No syscall filtering, no namespace work.
- We do not inspect TLS payloads — we evaluate the *intended* call before
  it leaves the box.
- We do not centralise logs to a server (local journald-style log only).

---

## 7. Approval UI

The approver surface is the trickiest piece. Goal: zero context switches
between IDE and approval — see what's pending, decide, get back.

### Decision points

- Notifications with action buttons on macOS require a properly bundled,
  signed app using `UNUserNotificationCenter` (legacy `osascript display
  notification` does not support buttons; `terminal-notifier` is dying).
  → ship `vetterd` as a `.app` bundle with a menu-bar icon.
- Click the menu-bar icon → popover lists pending requests with the §8
  rendered summary, detail panel, and three actions:
  `Approve`, `Allowlist…` (opens pattern picker), `Reject`.
- Notifications are best-effort: they include a one-line summary and a
  single "Review" action that opens the popover. We don't try to make
  Approve/Reject work directly from the notification banner — too easy to
  fat-finger, and the banner doesn't show enough context.
- TUI fallback (`vet --interactive`) for SSH/CI/headless: blocks the CLI
  with a colorised render and `[a]llow / [A]llowlist / [d]eny` keys.

### Cross-platform

- Linux: `org.vetter.Vetterd` GTK menu-bar (or AppIndicator) + libnotify.
- Windows: WinRT toast + tray icon.
- All platforms get a local web UI at `http://127.0.0.1:<port>/pending`
  as a uniform fallback that the user can keep open in a tab.

### Audit log

Every decision (auto or human) is appended to `~/.local/state/vet/audit.log`
as JSON lines. `vet allow list --history` surfaces this.

---

## 8. Command parsers

Parser plugins implement:

```rust
trait CommandParser {
    fn name(&self) -> &str;            // "curl"
    fn parse(&self, argv: &[String], stdin: Option<&[u8]>) -> Parsed;
    fn render(&self, p: &Parsed, w: &mut dyn StyledWriter) -> fmt::Result;
    fn match_rule(&self, p: &Parsed, rule: &RuleWhen) -> bool;
}
```

### curl parser (MVP)

Normalises curl argv into:

```
Parsed::Curl {
    method: Method,            // GET inferred from absence of -X/-d/-T
    url: Url,                  // scheme/host/port/path/query split
    headers: Vec<Header>,      // -H values, plus implicit (Content-Type from
                               //   -d, User-Agent if not overridden)
    body: BodyKind,            // None / Inline(bytes) / FromFile(path) /
                               //   FromStdin(digest) / Form(fields)
    auth: AuthKind,            // None / Basic / Bearer-from-header /
                               //   --user / netrc
    follow_redirects: bool,
    insecure: bool,            // -k / --insecure
    output: OutputKind,        // stdout / file / -O / -J
    upload_file: Option<Path>, // -T
    proxy: Option<Url>,
    pipes_to_shell: bool,      // best-effort: detect "| sh" sibling
                               //   when invoked via a shell wrapper
    raw_argv: Vec<String>,     // for display fallback
}
```

### Render layout (consistent regardless of curl flag order)

```
 vet  curl
 ─────────────────────────────────────────────────────────────────
 ⚠ POST  https://api.example.com/v1/users/42      [insecure: -k]
                              ──────────── path scrutinised
   Authorization: Bearer ••••f3a2          ← redacted, len 64
   Content-Type:  application/json
   X-Api-Key:     ••••                      ← flagged: secret
   User-Agent:    curl/8.4.0
 ─────────────────────────────────────────────────────────────────
   Body  (application/json, 312 B)
   { "email": "a@b.com", "role": "admin" }
 ─────────────────────────────────────────────────────────────────
   Risk signals: write-method, auth-header, --insecure
   Match:        no rule
   Action:       prompting approver…
```

Color scheme (rough):
- method: `GET/HEAD` green, `POST/PUT/PATCH` yellow, `DELETE` red, other magenta
- secret-bearing headers: red label, value redacted past last 4 chars
- non-https or `-k`: red badge
- localhost / loopback: dim cyan
- matched rule: green; no match: yellow; denylist: red

---

## 9. Safety signal heuristics

These never auto-deny by themselves; they bias the UI toward "human must
look" by suppressing auto-allow even if a permissive rule exists, unless
the rule explicitly opts in.

- write methods: POST/PUT/PATCH/DELETE
- auth-bearing headers: `Authorization`, `Cookie`, `X-*-Token`, `X-Api-Key`,
  `Proxy-Authorization`
- file uploads: `-T`, `-F file=@…`, `--data-binary @…`
- output to disk: `-o`, `-O`, `-J` outside cwd
- TLS off / verification disabled: `-k`, `--insecure`, `http://` non-loopback
- pipe to shell pattern (parent shell command contains `| sh|bash|zsh`)
- non-standard ports
- IDN / punycode hosts; raw IP literals

---

## 10. Tech-stack proposal

- **Language: Rust** for both `vet` and `vetterd`.
  - Single static binary, fast cold start (matters: `vet` is in the hot path
    of every wrapped call).
  - Mature `clap`/`argh` for arg parsing; `serde` for the wire protocol.
  - Native macOS UI via a thin Swift helper bundled in the `.app`,
    communicating with `vetterd` over the same Unix socket. Keeps Rust core
    portable and platform-specific code minimal.
- **IPC**: length-prefixed JSON over Unix socket. gRPC is overkill.
- **Config**: YAML for human-edited allowlist files; serde for
  (de)serialisation. JSON for the wire format.
- **Tests**: golden-file snapshots for the render output; property tests
  over rule matching; integration tests that spawn the daemon under
  `tempdir` socket.

(Open to Go if we decide the macOS native UI cost in Rust is too high; Go
makes the `.app`/Swift bridge slightly easier to maintain, at the cost of
slower CLI cold start and worse parser ergonomics.)

---

## 11. Roadmap

### Phase 0 — Skeleton (1–2 days)
- Cargo workspace: `vet` (cli), `vetterd` (daemon), `vetter-core` (parsers,
  policy, wire types).
- CI: fmt, clippy, tests, cross-platform build matrix.
- `vet doctor` stub.

### Phase 1 — Curl parser + standalone CLI (3–5 days)
- Full curl argv parser w/ snapshot tests against a corpus of real
  invocations harvested from agent transcripts.
- Renderer with color, redaction, signal flags.
- `vet --explain curl …` works end-to-end with no daemon, no policy.

### Phase 2 — Allowlist + TTY approval (3–5 days)
- YAML schema + loader for project + user scope.
- Rule matcher; unit + property tests.
- TTY blocking prompt fallback.
- `vet curl …` is now usable end-to-end, single-process.

### Phase 3 — Daemon + IPC (3–5 days)
- `vetterd` skeleton, socket protocol, request queue.
- `vet` switches from in-process eval to daemon eval, with TTY fallback
  retained.
- Audit log.

### Phase 4 — macOS approver UI (1–2 weeks)
- Swift menu-bar `.app` bundle, `UNUserNotificationCenter` integration,
  popover with pending list, detail view, action buttons.
- Code signing + notarisation pipeline. (this is the long pole)
- Local 127.0.0.1 web fallback.

### Phase 5 — Pattern suggestions (2–4 days)
- Generalisation engine (host, path-glob, method buckets).
- UI affordance to pick a generalisation when allowlisting.

### Phase 6 — Linux + Windows UI (1 week each, can be parallel)
- libnotify + AppIndicator on Linux.
- WinRT toast + tray icon on Windows.

### Phase 7 — Additional command parsers
- `wget`, `gh`, `aws`, `gcloud`, `ssh/scp`, `rm`, `git push`/`git remote`
  (mostly to gate `git push` to unfamiliar remotes).
- Each is roughly 1–3 days for parser + render + signals + tests.

---

## 12. Open questions

1. **Wrap vs. shim curl.** Brief mentions "wrap or replace curl". A PATH
   shim that intercepts bare `curl` would catch agents that forget to type
   `vet`, but breaks scripts and is surprising. Recommend explicit `vet
   curl …` for v1; revisit a shim mode (`vet install --shim curl`) later.
2. **Streaming requests.** curl can stream bodies (`-T` from stdin, chunked
   uploads). We can hash stdin up to a cap (e.g. 1 MiB) and prompt
   beyond that, but this is awkward. First pass: refuse to vet stdin >
   cap unless an explicit `streaming: true` rule matches.
3. **Allowlist semantics for query strings.** Are query params part of the
   match surface? Default off (most are non-sensitive), with an opt-in
   `query:` matcher.
4. **Multi-user / shared dev box.** Daemon is per-user; sockets in
   `$XDG_RUNTIME_DIR`. No cross-user policy.
5. **Latency budget.** Daemon round-trip target <30 ms on cache hit. Rust
   + Unix socket should be fine; needs a benchmark before we believe it.
6. **Telemetry.** None by default. Opt-in local-only metrics for
   diagnosing the project's own behavior.
7. **Distribution.** Homebrew tap for macOS, prebuilt binaries on GitHub
   Releases, AUR for Arch, scoop for Windows. Notarised `.app` for the
   daemon on macOS.

---

## 13. Success criteria

- An agent harness with `vet *` allowlisted and `curl|wget|gh|aws|… *`
  denied can complete normal development tasks with **fewer prompts than
  today** (because vetted patterns auto-allow) but **catches** any
  off-pattern HTTP call (because non-matching calls escalate to the human
  via the separate UI).
- The human can render an unfamiliar curl invocation as a structured,
  colorised summary in under one screen and decide in <10 seconds.
- Allowlist rules are diffable, reviewable, and shareable via the project
  repo.
- Adding a new command parser takes <1 day for the common case.
