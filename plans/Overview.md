# vetter — Overview

`vetter` is a local security gate that sits between an LLM coding agent and
"dangerous" CLI commands. The CLI is named `vet`. Agents are configured to
allowlist `vet *` (instead of `curl *`, `wget *`, …); `vet` then runs
its own, more sophisticated approval policy — backed by a separate-channel UI
operated by the human approver, not by the agent.

The MVP wraps `curl`. The architecture is built so additional command-aware
parsers (wget, httpie, …) can be added for other HTTP clients.

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
  "argv": ["-X", "POST", "https://…", "-d", "@/tmp/payload.json"],
  "parsed": { /* normalised command-specific struct, see §8 */ }
}
// Stdin-sourced bodies (`-d @-`, `-T -`, etc.) are rejected by the
// curl parser before the wire frame is built (ThreatModel T9), so
// the daemon never sees a request whose body depends on the
// agent-side pipe. v2 of the wire dropped `parsed` entirely; the
// daemon re-parses `argv` itself.

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
4. **Session scope** — disk-persisted like every other rule, self-describing
   via two optional `Rule` fields rather than a dedicated file or wire
   message: `expires_at` (Unix epoch seconds; the rule stops matching once
   `now >= expires_at`) and `sid` (a POSIX session id; the rule only
   matches requests whose caller shares that session, resolved server-side
   by [`peer_cred::stable_session_for`](../vetter-core/src/peer_cred.rs)
   walking the connecting process's ancestry to the nearest tty-anchored
   session — see that module's doc comment for the algorithm). Rules with
   either field set are written into the *same* `~/.vet/allowlist.yaml` /
   `<repo>/.vet/allowlist.yaml` files as permanent rules — the popover's
   duration picker ("15 minutes" / "1 hour" / "4 hours" / "for this
   terminal session" / "Forever") calls the identical
   `add_allowlist_rule(User)` path regardless of duration, only the two
   fields differ. `load_default()` partitions whatever it reads off disk
   into this tier at load time (`expires_at.is_some() || sid.is_some()`),
   so which physical file a rule came from no longer determines its
   precedence tier. This means session-scoped rules **survive a daemon
   restart** (expiry is wall-clock/identity based, not tied to process
   lifetime) — a deliberate improvement over "discarded on daemon
   restart", and also means `vet allow list` / `vet allow rm <id>` and
   `vet --explain` see them for free, no separate CLI surface needed.
   There is no background reaper: every rule-add (`allow_loader::add_rule`)
   lazily strips already-expired rules from the file before writing, and a
   pure `sid`-scoped rule (no `expires_at`, "for this session" with no
   fixed duration) gets a generous 7-day backstop `expires_at` from the
   picker purely so it's eventually reaped — the `sid` check already stops
   it from *matching* long before that.
5. **Denylist** — same shape, takes precedence over allows at every layer.

### Rule shape

Rules match on **effects** (see §8.2), not on a command-specific shape.
That means the same rule can cover any command that emits an
`HttpRequest` effect (curl, wget, httpie, …). An optional top-level
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

`when` clauses are keyed by effect kind that's specific to the
parser that produced it (`http:` for curl / wget / httpie). A rule
matches iff *every* populated effect-clause finds *at least one*
matching effect in the parsed command, and any populated `command:`
filter agrees. Anything not listed in `headers_allow` (or set to
`*`) is a mismatch — i.e. **default-deny on header surface**,
because Authorization, Cookie, X-Api-Key, etc. are exactly what we
want to scrutinise.

File paths (`Effect::FileRead` / `Effect::FileWrite`) are evaluated
through a separate cross-cutting layer — see [File-path
safe-paths layer](#file-path-safe-paths-layer) below. Rules in
`allowlist.yaml` do not carry file-path clauses.

Any rule (in either `rules:` or `deny:`, in any of the three files
above) may also carry `expires_at: <unix-epoch-seconds>` and/or
`sid: <i32>` — see "Session scope" above. These aren't restricted to
popover-authored rules; a human can hand-write them too, e.g. to
give a temporary teammate's onboarding rule a natural expiry.

### Known-hosts list

Alongside the allowlist, `vet` maintains a **known-hosts list** — a
catalogue of host patterns the user has declared "familiar". It lives in the
same `.vet/` directory with the same layered discovery:

- **Built-in** — baked into the binary; a curated short list of well-known
  public APIs (npm, PyPI, crates.io, GitHub, major cloud platforms, LLM APIs).
- **User scope** — `~/.vet/known-hosts.yaml`.
- **Project scope** — `<repo>/.vet/known-hosts.yaml`.

File format:

```yaml
hosts:
  - pattern: "api.github.com"
    note: "GitHub REST API"
  - pattern: "*.googleapis.com"
    note: "Google Cloud APIs"
```

Patterns follow the same glob rules as allowlist `url.host` fields: exact
(case-insensitive) or leading `*.` wildcard for subdomains (the apex itself
requires a separate entry).

**Listing a host does not grant any permission.** The known-hosts list is
purely a signal source. When an `HttpRequest` targets a host that is absent
from all layers, the risk analyzer emits an `UnknownHost` signal (see §9) —
a yellow warning that prompts human review without auto-denying.

### Pattern suggestions

The daemon ships two parallel suggestion engines, both reachable from
per-card `Allowlist…` / `Trust host…` actions on every pending and
Allow-resolved popover card:

**Allowlist generalisation** (HTTP requests). For the request's first
`Effect::HttpRequest`, propose 1–3 rules ordered tightest → loosest:

1. Exact match (host + method + full path).
2. Path-glob generalisation (last segment → `*`; emitted only when
   the request path has at least two segments — single-segment
   paths reduce to the method+host tier anyway).
3. Method-and-host only (`path: /**`).

**Known-host trust** (host pattern). For the request's host, propose
1–2 trust patterns:

1. Exact host (`api.example.com`).
2. Wildcard parent domain (`*.example.com`). Emitted only when the
   host has ≥3 labels and the leftmost is not `www`; loopback
   hosts and IP literals are skipped entirely.

Both engines live in `vetter-core::suggest` so the macOS picker and
any future `vet allow suggest` CLI share one implementation. Rule
ids are derived deterministically from the rule body
(`vetter_core::matcher::derive_auto_id`) so re-clicking the same tier
is idempotent.

Adding an allowlist rule that covers a currently-pending request
auto-resolves the matching pending entries with `Allow` and an
"auto-approved by newly added rule `<id>`" reason. Adding a known
host never auto-approves anything (known-host membership only
affects the `UnknownHost` signal, not the policy decision) but does
refresh the popover so the host pill flips orange → green.

Picker UI design: the v1 popover hands picker selection through an
`NSAlert` with a vertical `NSStackView` of radio buttons (one per
tier) plus a YAML preview of the rule body — see
[ApprovalUI.md §1.4](ApprovalUI.md). New rules / known hosts default
to the user scope (`~/.config/vet/allowlist.yaml`,
`~/.vet/known-hosts.yaml`) in v1; project-scope writes still work
through the existing `vet allow add --scope project` CLI.

### File-path safe-paths layer

File policy lives in a separate, command-agnostic layer alongside
the rule allowlist and the known-hosts list. The on-disk file is
`safe-paths.yaml`, layered identically (built-in / user / project
/ session) and discovered by walking up from `cwd` to a `.git`
boundary just like `allowlist.yaml`:

```yaml
allow:
  read:
    - path: "${HOME}/Downloads/**"
    - path: "/etc/ssl/**"
  write:
    - path: "/tmp/**"
deny:
  write:
    - path: "${HOME}/.ssh/**"
```

Every `Effect::FileRead` / `Effect::FileWrite` on a parsed command
is checked against this store independently of which command parser
produced the effect, then combined with the rule allowlist's
verdict: a deny on either side wins; an unknown path on the file
side downgrades a rule allow to a prompt; otherwise the rule
verdict carries. A built-in baseline auto-allows common scratch
and cache directories so routine writes to `${TMPDIR}` /
`~/.cache/**` don't prompt; a built-in deny list blocks writes to
sensitive credential paths (`~/.ssh/**`, `~/.aws/credentials`,
etc.) at every scope. See [FilePaths.md](FilePaths.md) for the
full strategy, the decision combinator, and the suggestion engine
that backs the popover's `Allow path…` button.

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

## 7. Approval UI — per-platform

The approver surface is a separate-channel UI: a different process, a
different window, owned by the human. The agent's terminal never sees
it. v0.1 shipped macOS only; v0.2 adds Ubuntu / Linux. The wire
protocol (§3) is platform-agnostic, so the per-platform code is
contained to `vetterd/src/notifier/<os>.rs` plus
`vetterd/src/runloop/<os>/`.

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

Operational guide: [MacOSApp.md](MacOSApp.md). Card catalogue:
[ApprovalUI.md](ApprovalUI.md).

### Linux (Ubuntu 22.04+)

- `vetterd` runs as a `systemd --user` service, started at login by
  `vetter.service` (installed under `/usr/lib/systemd/user/` by the
  `.deb`). The service is the Linux equivalent of the macOS `.app`
  bundle: it owns the lifecycle, sets `VETTERD_NOTIFIER=linux` in
  the unit `Environment`, and integrates with journald.
- Approval surface uses `org.freedesktop.Notifications` over D-Bus
  (via the `zbus` crate — no libnotify C dep). Each pending request
  raises a notification with `Approve` and `Reject` actions; the
  `ActionInvoked` signal feeds the same `PendingQueue::resolve`
  path the macOS notification delegate uses. `Allowlist…` lives on
  the popover card rather than the notification banner because
  D-Bus notification action support is uneven across daemons.
- A system-tray icon (StatusNotifierItem via the `ksni` crate)
  shows a shield + pending-count badge. Clicking it opens a GTK4
  popover window whose card layout mirrors macOS card-for-card:
  URL row, signal pills, effect rows, Show raw, Allowlist… /
  Trust host… picker sheets. The data layer
  (`PromptSummary` → card fields) is shared with macOS;
  `vetterd/src/runloop/linux/` only owns the GTK widget assembly
  and `pango::AttrList` translation of the `Style` SGR taxonomy
  in [ApprovalUI.md](ApprovalUI.md) §"Body colouring".
- Notifications coalesce after the first banner per the same
  `NotifyHint::was_empty_before` mechanism as macOS.
- **Headless / SSH path.** When `$DBUS_SESSION_BUS_ADDRESS` is
  unset (CI, containers, SSH without a graphical session), the
  daemon refuses to install the `linux` notifier and exits with
  code 78 — the same fail-closed shape as the macOS bundle guard.
  Users who legitimately want to drive the daemon from a TTY set
  `VETTERD_NOTIFIER=noop` and resolve requests via the admin
  socket: `vet daemon list` to enumerate, `vet daemon approve <id>`
  / `vet daemon reject <id>` to resolve. The admin protocol is
  the same `vetter-admin.sock` that backs `vet daemon status`.

Operational guide: [LinuxApp.md](LinuxApp.md). Distribution flow:
[Release.md](Release.md) §"Linux / Launchpad PPA".

### Other platforms

Windows (WinRT toast + tray) and a localhost web UI remain on the
backlog (TODO.md *Backlog → Phase 8*). Both reuse the existing wire
protocol and the platform-agnostic card-data layer; only the UI
shell is new work.

### Audit log

Every decision (auto or human) is appended to
`~/Library/Logs/vetter/audit.log`
as JSON lines. `vet allow list --history` surfaces this.

---

## 8. Command parsers

The parser layer is the project's most important extension point. Adding
a new HTTP-client command (wget, httpie, …) should require **only** a new
parser plugin — the renderer, the rule matcher, the risk analyzer, the
audit log, and the UI all consume one shared output type and need no
per-command code.

### 8.1 Plugin contract

A parser is the *only* per-command code. It owns argv → structured
output and nothing else.

```rust
pub trait CommandParser: Send + Sync {
    /// Stable identifier ("curl", "wget", …). Appears in rules,
    /// audit logs, and the wire protocol.
    fn name(&self) -> &'static str;

    /// True if this parser handles the given argv[0] (incl. basenames
    /// like "/opt/homebrew/bin/curl"). Plugins may also accept aliases
    /// (e.g. a future httpie plugin returning true for both "http" and "https").
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
    FileWrite(FileWrite),     // -o /path, -O, wget default save
    FileRead(FileRead),       // -d @file, -T file
    ProcessSpawn(ProcessSpawn),
    CredentialUse(CredentialUse), // --user, ~/.netrc

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
                              //   Form(Vec<FormField>)
                              // Stdin-sourced bodies are rejected
                              // in the parser (ThreatModel T9):
                              // agents must use a temp file.
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
  (e.g. curl's `--write-out` format string).
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
//   curl, wget, httpie — all handled identically.
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
   Authorization: ••••f3a2    ← redacted, len 64
   Content-Type:  ••••json    ← redacted, len 16
   X-Api-Key:     ••••        ← redacted, len 3
   User-Agent:    ••••8.4.0   ← redacted, len 10
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
- header values: every value is redacted past last 4 chars (the
  renderer treats every value as sensitive — see §8.5.1); known
  auth-bearing headers also drive the `auth-header` risk signal
- non-https or `-k`: red badge
- localhost / loopback: dim cyan
- matched rule: green; no match: yellow; denylist: red

#### 8.5.1 Header value redaction

Every header value is replaced with `••••<last4>` plus a dim
`← redacted, len N` suffix before reaching the writer. There is
no allowlist of "harmless" header names: any value can carry
secrets we wouldn't recognise (custom auth headers like
`X-Tenant-Token-V2`, signed S3 URLs in `Referer`, JWTs in
non-standard places, tenant identifiers, etc.), and "guess wrong,
leak the value" is a worse failure mode than "always redact,
occasionally hide a benign value". The §8.5 layout therefore
surfaces *which* headers a request carries, but never their
contents.

The list of header names that count as "auth-bearing" still
exists, but only as a risk-signal heuristic: see §9 below and
[`vetter_core::signals::is_auth_header`](../vetter-core/src/signals/mod.rs).
A request that carries one of those headers fires the
`auth-header` signal (green pill in the popover, listed under
`Risk signals:` in the §8.5 layout); the value itself is still
redacted by the same recipe as every other header.

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
- unknown host: the requested host is absent from every layer of the
  known-hosts list (§5 "Known-hosts list"), signalling that the agent is
  reaching somewhere the user has not explicitly recognised as familiar;
  loopback addresses are always exempt
- follow-redirects: `-L`/`--location` enabled; auto-allow rules must
  explicitly opt in via `http: { no_redirects: false }` to permit
  redirect-following requests (default-deny so an allowlisted host that
  open-redirects, or is compromised, cannot pivot the request to an
  arbitrary origin under the original auto-allow)

Generic (over `Effect::FileWrite` / `FileRead`):
- file outside the allowlist's known-safe directories (`UnknownWritePath` /
  `UnknownReadPath`) — see [FilePaths.md](FilePaths.md) for the full
  baseline and denylist strategy
- path explicitly blocked by the built-in denylist (`DeniedPath`)

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

### Phase 5 — Pattern + known-host suggestions (2–4 days)
- Generalisation engine in `vetter-core::suggest`: allowlist tiers
  (Exact / PathGlob / MethodHost) + known-host tiers (Exact /
  Wildcard).
- `vetter-core::known_hosts` write API mirroring `matcher::loader`
  (atomic temp + rename, case-insensitive dedup).
- Admin protocol: `MgmtRequest::{SuggestionsFor, AddRule,
  AddKnownHost}` + matching responses.
- Daemon: shared `Arc<RwLock<…>>` stores; `vetterd::suggestions`
  module wires admin handlers into auto-approve + signal-refresh.
- macOS popover: per-card `Allowlist…` / `Trust host…` buttons
  open `NSAlert` picker sheets backed by the suggestion engine.

### Phase 6 — Ubuntu support (v0.2 milestone)
- D-Bus desktop notifications via `zbus` with Approve / Reject actions
  (no libnotify C dep). Coalesce after first banner, mirroring macOS.
- StatusNotifierItem tray (via `ksni`) with shield icon + pending-count
  badge + "Open vetter window…" / "Pending: N" / "Quit Vetter" menu.
- GTK4 popover window mirroring the macOS card catalogue card-for-card
  (URL row, signal pills, effect rows, Show raw, Allowlist… / Trust
  host… picker sheets). Card-data lowering is shared with macOS;
  `vetterd/src/runloop/linux/` only owns widget assembly + SGR-to-
  `pango::AttrList` translation.
- `.deb` packaged via `cargo-deb`; ships `/usr/bin/{vet,vetterd}`,
  `/usr/lib/systemd/user/vetter.service`,
  `/etc/xdg/autostart/vetter.desktop`, and a hicolor SVG tray icon.
  `postinst` runs `systemctl --user --global enable vetter.service`.
- Distribution: Launchpad PPA (`ppa:blevinstein/vetter`), GPG-signed
  source upload via `dput`. End-user install:
  `sudo add-apt-repository ppa:blevinstein/vetter && sudo apt-get
  install vetter`. See [Release.md](Release.md) §"Linux / Launchpad
  PPA".
- Headless / SSH: new admin commands `vet daemon approve <id>` and
  `vet daemon reject <id>` over the existing `vetter-admin.sock` so
  users can resolve from a TTY when no D-Bus session is available.

### Phase 7 — Additional HTTP-client parsers
- `wget`, `httpie` (`http`/`https`). Only commands whose primary
  purpose is making HTTP requests map cleanly onto the existing
  allowlist model (URL scheme/host/port/path, method, headers, body,
  TLS policy, redirects). Non-HTTP commands (`ssh`, `rm`, `git push`,
  etc.) would need entirely different rule schemas, signal heuristics,
  and approval UIs; they are out of scope.
- Each is one new file implementing `CommandParser`, plus snapshot
  fixtures and any command-specific signal additions. No core changes.
  Budget: 1–3 days each. The first one (`wget`) doubles as a check
  that the Phase 1a interface generalised correctly; if it forces
  changes to `Effect`/`ParsedCommand`, that's a useful signal we
  under-designed and should fix before the rest land.

### Phase 8 — Other platforms (post-v0.2, opportunistic)
- Windows: WinRT toast + tray.
- localhost web UI (`http://127.0.0.1:<port>`) as a uniform option for
  remote / chrome-OS-style environments.
- Both reuse the existing socket protocol and the platform-agnostic
  card-data layer; only the UI shell is new work.

---

## 12. Open questions

1. **Allowlist semantics for query strings.** Are query params part of the
   match surface? Default off (most are non-sensitive), with an opt-in
   `query:` matcher.
2. **Telemetry.** None by default. Opt-in local-only metrics for
   diagnosing the project's own behavior.
3. **Distribution.** v0.1 ships macOS only via a Homebrew tap
   delivering the notarised `.app` (which contains both `vet` and
   `vetterd`), with symlinks into `/usr/local/bin` for the CLI. v0.2
   adds Ubuntu via a Launchpad PPA delivering a `.deb` that installs
   `/usr/bin/{vet,vetterd}` plus a systemd user unit. Windows
   packaging waits for Phase 8.

---

## 13. Success criteria

- An agent harness with `vet *` allowlisted and `curl|wget|… *`
  denied can complete normal development tasks with **fewer prompts than
  today** (because vetted patterns auto-allow) but **catches** any
  off-pattern HTTP call (because non-matching calls escalate to the human
  via the separate UI).
- The human can render an unfamiliar curl invocation as a structured,
  colorised summary in under one screen and decide in <10 seconds.
- Allowlist rules are diffable, reviewable, and shareable via the project
  repo.
- Adding a new command parser takes <1 day for the common case.
