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
2. **User scope** — `~/.vet/allowlist.yaml`. Personal, never commited.
3. **Project scope** — `<repo>/.vet/allowlist.yaml`. Discovered by walking
   up from `cwd` to a directory containing this file or `.git`. Designed to
   be checked in so a team shares vetted patterns.
4. **Session scope** — in-memory only, populated by `allow_once` decisions
   from the UI. Discarded on daemon restart.
5. **Denylist** — same shape, takes precedence over allows at every layer.

### Rule shape

Rules match on **effects** (see §8.2), not on a command-specific shape.
That means the same rule can cover any command that emits an
`HttpRequest` effect (curl, wget, gh, …). An optional top-level
`command:` field can narrow a rule to one parser when needed.

```yaml
rules:
  - id: github-readonly
    # No `command:` — applies to any parser whose effects match.
    when:
      http:
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
    when:
      http:
        url:
          host: ["localhost", "127.0.0.1", "::1"]
          port: [3000, 8000, 8080]
    note: "Local dev servers"

  - id: scoped-curl-only
    command: curl              # narrow when we genuinely mean curl
    when:
      http: { method: [GET] }
```

`when` clauses are keyed by effect kind (`http:`, `file_write:`,
`file_read:`, `process_spawn:`, …). A rule matches iff *every*
populated effect-clause finds *at least one* matching effect in the
parsed command, and any populated `command:` filter agrees.
Anything not listed in `headers_allow` (or set to `*`) is a mismatch
— i.e. **default-
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

See [ThreatModel.md](ThreatModel.md) for the unmitigated attacks we
plan to fix and their code sites.

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

The parser layer is the project's most important extension point. Adding
a new command (wget, gh, aws, ssh, …) should require **only** a new
parser plugin — the renderer, the rule matcher, the risk analyzer, the
audit log, and the UI all consume one shared output type and need no
per-command code.

### 8.1 Plugin contract

A parser is the *only* per-command code. It owns argv → structured
output and nothing else.

```rust
pub trait CommandParser: Send + Sync {
    /// Stable identifier ("curl", "wget", "gh", …). Appears in rules,
    /// audit logs, and the wire protocol.
    fn name(&self) -> &'static str;

    /// True if this parser handles the given argv[0] (incl. basenames
    /// like "/opt/homebrew/bin/curl"). Plugins may also accept aliases
    /// (e.g. a future gh plugin returning true for both "gh" and "hub").
    fn handles(&self, argv0: &str) -> bool;

    /// Parse argv into the shared output model. Returning Err means
    /// the invocation is unparseable; `vet` then refuses to run it
    /// rather than guessing — refusing is safer than mis-vetting.
    fn parse(
        &self,
        argv: &[String],
        stdin: StdinHandle,    // bounded reader; see §8 streaming note
        env: &EnvSnapshot,     // read-only; parsers may inspect e.g.
                               //   CURL_HOME, AWS_PROFILE
    ) -> Result<ParsedCommand, ParseError>;
}
```

Plugins are registered via a small static registry:

```rust
pub fn register(parser: Box<dyn CommandParser>);
pub fn dispatch(argv: &[String]) -> Option<&dyn CommandParser>;
```

For MVP we register parsers explicitly in `vetter-core::parsers::all()`;
no dynamic loading. (Dynamic loading is a future option but adds a
trust-boundary problem we don't need yet.)

### 8.2 Shared output model — `ParsedCommand`

Every parser produces the same shape. The shape is built around
**effects** — concrete, normalized things the command will do — plus
metadata for display and a small command-specific escape hatch.

```rust
pub struct ParsedCommand {
    pub command: &'static str,            // parser.name()
    pub argv: Vec<String>,                // raw, preserved for display
    pub stdin_digest: Option<Sha256>,
    pub effects: Vec<Effect>,             // the heart of the model
    pub signals: Vec<RiskSignal>,         // pre-computed by the parser
    pub display_hints: DisplayHints,      // method/target/etc. for the
                                          //   header line in the UI
    pub extras: serde_json::Value,        // command-specific extras
                                          //   (raw, for the detail view)
}
```

The point of `effects` is that downstream consumers — the rule matcher,
the risk analyzer, the renderer's "summary" view — never branch on
`command`. They iterate effects.

```rust
pub enum Effect {
    HttpRequest(HttpRequest),
    FileWrite(FileWrite),     // -o /path, -O, scp dst, gh pr download
    FileRead(FileRead),       // -d @file, -T file, scp src
    ProcessSpawn(ProcessSpawn), // ssh remote command, gh codespace ssh
    CredentialUse(CredentialUse), // --user, ~/.netrc, AWS_PROFILE…
    Network(NetworkOpen),     // raw socket-y things (ssh, nc) where
                              //   "HTTP request" doesn't apply
}

pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: Url,             // scheme/host/port/path/query split
    pub headers: Vec<Header>, // includes parser-inferred (e.g. curl's
                              //   implicit Content-Type from -d)
    pub body: Body,           // None / Inline(bytes) /
                              //   FromFile(PathBuf) /
                              //   FromStdin(Sha256, len) /
                              //   Form(Vec<FormField>)
    pub auth: Option<Auth>,   // Basic / Bearer / Header(name) / Netrc
    pub tls: TlsPolicy,       // Strict / InsecureSkipVerify / Plaintext
    pub follow_redirects: bool,
    pub proxy: Option<Url>,
}

pub struct FileWrite {
    pub path: PathBuf,        // absolute, normalized
    pub source: WriteSource,  // RemoteHttp(Url) / Stdin / Literal(bytes)
    pub overwrite: bool,
}

// … FileRead, ProcessSpawn, etc. follow the same pattern.

pub struct RiskSignal {
    pub kind: SignalKind,     // enum: WriteMethod, AuthHeader,
                              //   InsecureTls, PipeToShell, …
    pub detail: String,       // human-readable specific (e.g.
                              //   "Authorization header present")
    pub effect_idx: Option<usize>, // which effect raised this
}

pub struct DisplayHints {
    pub primary_verb: String,    // "POST", "scp →", "ssh"
    pub primary_target: String,  // "https://api.example.com/v1/users/42"
    pub badges: Vec<Badge>,      // "[insecure: -k]", "[stdin]", …
}
```

Why both `effects` and `extras`:
- `effects` is the **structured surface** that policy and analysis run
  on. Anything we want to write a generic rule against goes here.
- `extras` is a **lossless escape hatch** for command-specific detail
  the renderer's detail view can show but policy doesn't reason about
  (e.g. curl's `--write-out` format string, gh's repo-context flags).
  Keeping it free-form means parsers can ship richer detail without
  forcing a core-type change every time.

This shape is also the wire format: `ParsedCommand` `serde`-derives
JSON and is what the CLI sends to the daemon (§3 `VetRequest.parsed`).

### 8.3 Downstream consumers (all command-agnostic)

```rust
// Renderer: turns ParsedCommand into the colorised UI block (§8.5).
//   The summary header is built from DisplayHints; the body iterates
//   effects with per-Effect-variant renderers in vetter-core. Parsers
//   never render anything.
pub trait Renderer {
    fn render(&self, p: &ParsedCommand, w: &mut dyn StyledWriter);
}

// Matcher: rule WHEN clauses are written against effects.
//   "method: POST, host: api.github.com" matches any ParsedCommand
//   whose effects contain an HttpRequest matching the predicate —
//   curl, wget, gh-with-an-http-effect, all handled identically.
pub trait Matcher {
    fn matches(&self, p: &ParsedCommand, rule: &RuleWhen) -> bool;
}

// Risk analyzer: pure function over effects.
pub fn analyze(p: &ParsedCommand) -> Vec<RiskSignal>;
```

Adding wget therefore touches one file (`parsers/wget.rs`) plus its
test fixtures. No core changes.

### 8.4 curl parser (MVP)

The first concrete `CommandParser`. Responsibilities:

- Argv parser covering the curl flag set we care about (full list lives
  in `parsers/curl/flags.rs`; snapshot-tested against a corpus of real
  invocations harvested from agent transcripts).
- Emits a single `Effect::HttpRequest` (plus an `Effect::FileWrite` if
  `-o`/`-O`/`-J` is used, plus an `Effect::FileRead` for `-T <file>`
  or `-d @file`).
- Populates `signals` with curl-specific cues that the generic analyzer
  can't see: `--insecure`/`-k`, `--cacert` overrides, `--resolve`
  spoofing, etc.

**Streaming bodies are refused for MVP.** If stdin is a pipe whose total
size we cannot read into memory cheaply (cap: 1 MiB), or if `-T -` /
chunked-transfer flags are used, the parser returns `ParseError::
StreamingUnsupported`; `vet` exits non-zero with a clear message.
Lifting this is a future phase.

### 8.5 Render layout (consistent regardless of curl flag order)

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

Implemented as a pure function over `ParsedCommand.effects`, so signals
are command-agnostic by default. A parser may push extra
command-specific signals during `parse()` (e.g. curl's `--resolve`
spoofing).

Signals never auto-deny by themselves; they bias the UI toward "human
must look" by suppressing auto-allow even if a permissive rule exists,
unless the rule explicitly opts in.

Generic (over `Effect::HttpRequest`):
- write methods: POST/PUT/PATCH/DELETE
- auth-bearing headers: `Authorization`, `Cookie`, `X-*-Token`,
  `X-Api-Key`, `Proxy-Authorization`
- TLS off / verification disabled (`tls != Strict`, `http://` non-loopback)
- non-standard ports
- IDN / punycode hosts; raw IP literals

Generic (over `Effect::FileWrite` / `FileRead`):
- file upload of a path outside cwd
- output to disk outside cwd

Generic (over `Effect::ProcessSpawn`):
- pipe to shell pattern (parent shell command contains `| sh|bash|zsh`)

Curl-specific (parser-pushed): `--insecure`/`-k`, `--cacert` overrides,
`--resolve` host pinning, `--unix-socket`.

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
- Cargo workspace: `vet` (cli), `vetterd` (daemon), `vetter-core`
  (shared output model, plugin registry, renderer, matcher, signals,
  wire types).
- CI: fmt, clippy, tests; macOS build target only for MVP (Linux runner
  for headless tests of `vetter-core` is fine, but no Linux/Windows
  binary is shipped).
- `vet doctor` stub.

### Phase 1a — Plugin scaffolding (2–3 days)
- `CommandParser` trait, `ParsedCommand`, `Effect` enum + variants,
  `RiskSignal`, `DisplayHints`, `StdinHandle`, `EnvSnapshot` (all in
  `vetter-core`).
- Static parser registry + dispatch.
- Generic renderer that walks `effects` and produces the §8.5 layout
  from any `ParsedCommand` (no curl-specific code).
- Generic risk analyzer (§9 generic items only).
- A trivial `noop` parser used in tests to exercise the whole pipeline
  without depending on the curl parser.

### Phase 1b — Curl parser (3–5 days)
- Full curl argv parser w/ snapshot tests against a corpus of real
  invocations harvested from agent transcripts.
- Emits `Effect::HttpRequest` (+ optional `FileWrite`/`FileRead`) and
  curl-specific `RiskSignal`s.
- `vet --explain curl …` works end-to-end with no daemon, no policy,
  using the Phase 1a renderer and analyzer unchanged.

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
- Each is one new file implementing `CommandParser`, plus snapshot
  fixtures and any command-specific signal additions. No core changes.
  Budget: 1–3 days each. The first one (`wget`) doubles as a check
  that the Phase 1a interface generalised correctly; if it forces
  changes to `Effect`/`ParsedCommand`, that's a useful signal we
  under-designed and should fix before the rest land.

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
