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
  (`$TMPDIR/vetter.sock` on macOS).
- Sends a `VetRequest`, blocks on a `VetDecision`.
- On `allow`, `execvp`s the underlying command, inheriting stdio so behavior
  is indistinguishable from running `curl` directly.
- On `deny`, prints reason on stderr and exits with code `77` (sysexits
  `EX_NOPERM`).
- **Fails closed** if `vetterd` is not running: prints a clear "daemon not
  running, run `vet daemon start`" message and exits non-zero. We do not
  fall back to a TTY prompt — a blocking prompt in the agent's own
  terminal would defeat the separate-channel guarantee, since the agent
  can read its own stdout/stdin.

### `vetterd` (daemon, long-running, per-user)
- Owns the allowlist files, the in-memory policy, the pending-request queue,
  and the platform-specific approval UI.
- Listens on the Unix socket; one request → one decision.
- Persists allowlist mutations atomically.
- Surfaces pending approvals via the macOS native UI (§7). Other platforms
  will reuse the same socket protocol when their UI is added; until then
  `vet` is macOS-only.

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

There is intentionally **no `--interactive` / TTY prompt mode**. Approvals
always go through the daemon's separate-channel UI; if the daemon is down,
`vet` errors out.

**Opt-in only — no global hook.** `vet` does not install a PATH shim,
shell function, or any other interception layer that captures bare
`curl` invocations. The agent harness must be configured to invoke
`vet curl …` explicitly. This keeps the tool predictable and avoids
breaking unrelated scripts that expect plain `curl`.
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

## 7. Approval UI — macOS only for MVP

The approver surface is a separate-channel UI: a different process, a
different window, owned by the human. The agent's terminal never sees it.
For the MVP we ship one platform — macOS — and design the protocol so
other platforms drop in later without rework.

### macOS

- `vetterd` is shipped as a code-signed, notarised `.app` bundle with a
  menu-bar icon and no dock presence (`LSUIElement`).
- Approval surface uses `UNUserNotificationCenter`, which requires the
  bundled-app form (legacy `osascript display notification` doesn't
  support action buttons; `terminal-notifier` is unmaintained).
- Each pending request raises a notification with three actions:
  `Approve`, `Allowlist…`, `Reject`. The `Allowlist…` action opens a
  small window with the §5 generalisation choices.
- Clicking the notification body (or the menu-bar icon) opens a popover
  listing all currently-pending requests with the §8 rendered summary
  and a detail view (full headers, decoded body, risk signals).
- If multiple requests are queued, notifications coalesce into the
  menu-bar popover after the first; we don't spam banners.

### Other platforms

Out of scope for MVP. `vet` will refuse to run on non-macOS with a clear
"only macOS is supported in this release" message until we add a UI.
The wire protocol (§3) is platform-agnostic, so adding Linux (libnotify
+ AppIndicator), Windows (WinRT toast + tray), or a localhost web UI
later is a UI-only project — no daemon or CLI changes required.

### Audit log

Every decision (auto or human) is appended to
`~/Library/Logs/vetter/audit.log`
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

**Streaming bodies are refused for MVP.** If stdin is a pipe whose total
size we cannot read into memory cheaply (cap: 1 MiB), or if `-T -` /
chunked-transfer flags are used, `vet` errors out with a clear
"streaming requests are not supported yet" message and exits non-zero.
Lifting this restriction is left to a future phase.

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
- CI: fmt, clippy, tests; macOS build target only for MVP (Linux runner
  for headless tests of `vetter-core` is fine, but no Linux/Windows
  binary is shipped).
- `vet doctor` stub.

### Phase 1 — Curl parser + standalone CLI (3–5 days)
- Full curl argv parser w/ snapshot tests against a corpus of real
  invocations harvested from agent transcripts.
- Renderer with color, redaction, signal flags.
- `vet --explain curl …` works end-to-end with no daemon, no policy.

### Phase 2 — Allowlist evaluation (3–5 days)
- YAML schema + loader for project + user scope.
- Rule matcher; unit + property tests.
- `vet --explain curl …` reports the policy decision (allow/deny/prompt)
  without yet being able to surface a prompt to a human.

### Phase 3 — Daemon + IPC (3–5 days)
- `vetterd` skeleton, socket protocol, request queue, audit log.
- `vet` becomes a thin client of the daemon; fails closed if the socket
  is missing.
- Approver surface for this phase is a stub: any prompt-class decision
  is auto-denied with a "no UI yet" reason. Lets us land and test the
  whole pipeline before any Swift code exists.

### Phase 4 — macOS approver UI (1–2 weeks, the long pole)
- Swift menu-bar `.app` bundle, `LSUIElement`, no dock icon.
- `UNUserNotificationCenter` integration with Approve / Allowlist… /
  Reject actions.
- Popover with pending list and detail view (renders §8 output).
- Code signing + notarisation pipeline.
- This is the MVP-complete milestone: with Phase 4 landed, an agent
  harness with `vet *` allowlisted can do useful work end-to-end.

### Phase 5 — Pattern suggestions (2–4 days)
- Generalisation engine (host, path-glob, method buckets).
- "Allowlist…" action opens the picker UI from Phase 4.

### Phase 6 — Other platforms (post-MVP, opportunistic)
- Linux: libnotify + AppIndicator.
- Windows: WinRT toast + tray.
- localhost web UI (`http://127.0.0.1:<port>`) as a uniform option.
- All three reuse the existing socket protocol; no daemon/CLI changes.

### Phase 7 — Additional command parsers
- `wget`, `gh`, `aws`, `gcloud`, `ssh/scp`, `rm`, `git push`/`git remote`
  (mostly to gate `git push` to unfamiliar remotes).
- Each is roughly 1–3 days for parser + render + signals + tests.

---

## 12. Open questions

1. **Allowlist semantics for query strings.** Are query params part of the
   match surface? Default off (most are non-sensitive), with an opt-in
   `query:` matcher.
2. **Telemetry.** None by default. Opt-in local-only metrics for
   diagnosing the project's own behavior.
3. **Distribution.** MVP is macOS only: Homebrew tap delivering the
   notarised `.app` (which contains both `vet` and `vetterd`), with
   symlinks into `/usr/local/bin` for the CLI. Linux/Windows packaging
   waits until Phase 6.

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
