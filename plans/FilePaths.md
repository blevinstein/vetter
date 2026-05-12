# File-path safe-paths layer

Companion to [Overview.md](Overview.md) §5 and §9. This document specifies how
`vetter` evaluates `Effect::FileRead` and `Effect::FileWrite` effects against
a **separate, command-agnostic safe-paths layer** that runs alongside the
existing rule-based allowlist.

---

## 1. Problem

Today the only file-effect signals are `FileOutsideCwd` and
`FileReadOutsideCwd`
([`vetter-core/src/signals/mod.rs`](../vetter-core/src/signals/mod.rs)
`analyze_file_write` / `analyze_file_read`). This is a coarse binary:

- Any path outside `cwd` warns — `/etc/hosts`, `/tmp/curl-XXX`,
  `~/Downloads/foo.tar.gz` all produce identical yellow signals regardless
  of risk.
- No built-in distinctions between "safe scratch space" and "credential
  store". A write to `~/.ssh/authorized_keys` looks the same as a write to
  `/tmp/output.json`.
- The allowlist already ships `file_write:` / `file_read:` clauses on
  rules, but having them sit on each rule has the wrong granularity for
  what we actually want: most file-path policy is **cross-cutting** — the
  question of "may this command read `~/.ssh/id_rsa`?" has the same answer
  whether the command is `curl`, `wget`, `cat`, `git`, or `rm`. URL policy
  is the opposite — `url:` clauses only make sense for parsers that emit
  `Effect::HttpRequest` (curl, wget, httpie, …) and forcing every
  curl `http:` rule to also enumerate which file paths are OK to attach
  via `-d @file` is the wrong shape: it scales O(URLs × paths) rather than
  O(URLs + paths).

The result is approval fatigue for agents that routinely write to
`${TMPDIR}` or `~/Downloads` and, paradoxically, insufficient alarm
when a write targets a sensitive path like `~/.aws/credentials`.

---

## 2. Design

### 2.1 File paths live in their own layered store

A new module `vetter-core::safe_paths` mirrors the `known_hosts` module
structurally. It is a third pillar alongside `matcher` (rule allowlist)
and `known_hosts` (host catalogue), with the same layered discovery and
atomic write API.

The on-disk file is `safe-paths.yaml` with the same per-scope locations
the allowlist uses:

| Scope | Path | Source of truth |
|---|---|---|
| Built-in | compiled into `vetter-core` | this document, §6 |
| User | `~/.vet/safe-paths.yaml` | the user |
| Project | `<repo>/.vet/safe-paths.yaml` | the project, walked up from `cwd` to a `.git` boundary |
| Session | in-memory only | `vet allow add --session` style flows |

The store is consulted independently of the rule allowlist: every
`Effect::FileRead` / `Effect::FileWrite` on a parsed command is checked
against it, regardless of which command parser produced the effect. A
single `safe-paths.yaml` therefore applies to curl, wget, httpie,
and every future parser without duplicating policy.

This is the model the user explicitly asked for: "file paths modeled as
a separate layer."

### 2.2 `file_write:` and `file_read:` clauses leave the rule schema

The existing `RuleWhen.file_write` / `RuleWhen.file_read` clauses
([vetter-core/src/matcher/rule.rs](../vetter-core/src/matcher/rule.rs))
are deleted. A rule allowlist entry now describes **what command-level
operation** is permitted (today: HTTP requests; future: process spawns,
network listens, credential uses); whether the *paths* the command
touches are OK is answered by the safe-paths layer.

Rationale:

- The cross-cutting framing makes operator intent clearer: a single
  entry in `safe-paths.yaml` covers `~/Downloads/**` for every
  command, instead of having to add a `file_write:` clause to each
  curl / wget / httpie rule.
- Removes a long-standing footgun: today a rule with only `http:`
  populated silently auto-allows the file effects on the same
  command (clause omitted ⇒ effect unconstrained). Under the new
  model file effects always go through their own gate.
- Keeps the rule schema tight. With curl as the only parser today,
  the rule schema collapses to `command:` + `http:` + (future
  per-effect-type clauses); there is no clause-explosion as new
  parsers ship.

The conjunction case the old schema technically supported — "reading
`/tmp/Y` is OK *only* when also POSTing to X" — is no longer
expressible. We accept the trade-off: that pattern is rare in practice,
and operators who want it can express it by tightening the path glob
(e.g. add `/tmp/Y-for-X-only/**` and have the agent stage there).

### 2.3 Reads and writes stay separate

A path can be read-safe but never write-safe (e.g. `/etc/ssl/**`,
`~/.netrc`). Merging them into a single `path:` list would require
explicit direction qualifiers per entry and make the YAML harder to
review. The schema in §3 keeps `read:` and `write:` as sibling lists,
mirroring the existing `FileReadClause` / `FileWriteClause` split.

### 2.4 Built-in baseline stays minimal; users author the rest

Two competing pressures:

- **Approval fatigue is real.** Every coding agent writes to scratch
  directories on almost every task. If the daemon prompts on every
  `${TMPDIR}/curl-XXX` write the user never gets useful work done.
- **Baked-in policy is opinionated.** Listing `~/.ssh/**` as built-in
  deny, or `~/Downloads/**` as built-in allow, makes choices for the
  user that reasonable people disagree about (some users genuinely
  want their agent to write authorised_keys; some want Downloads
  off-limits). A security tool that ships strong opinions invites
  pull requests to relax them.

The compromise: the built-in baseline covers only what's
**mechanically necessary** for tools to function on the host
platform, plus the **single irreducible deny** that protects vetter's
own trust model. Everything else lives in a starter template the
user can copy verbatim (`vet allow init-safe-paths`) or ignore in
favour of authoring rules through the popover picker on first
prompt. §7 lists the actual contents.

Authorship is the primary mechanism. Three paths:

1. **Popover picker** — every `Unknown` path on a prompt surfaces an
   `Allow path…` button with Exact / Dir / ProjectRoot suggestions
   and writes user-scope `safe-paths.yaml` on click (§8).
2. **CLI** — `vet allow add --safe-read <path>` /
   `--safe-write <path>` for shell-driven workflows.
3. **Starter template** — `vet allow init-safe-paths` copies a
   curated `safe-paths.starter.yaml` (shipped in
   [`vetter-core/resources/`](../vetter-core/resources/)) into
   `~/.vet/safe-paths.yaml` so users who'd rather start from a
   reviewed default than build up from zero have one ready (§7.3).

---

## 3. Schema

`safe-paths.yaml` carries four sibling sections — `allow.read`,
`allow.write`, `deny.read`, `deny.write`. Each is a list of entries:

```yaml
allow:
  read:
    - path: "${HOME}/Downloads/**"
      note: "User-expected download destination"
    - path: "/etc/ssl/**"
      note: "TLS certificate store"
  write:
    - path: "/tmp/**"
      note: "Scratch writes"
    - path: "${HOME}/Downloads/**"

deny:
  write:
    - path: "${HOME}/.ssh/**"
      note: "SSH key material"
  read:
    - path: "${HOME}/.aws/credentials"
      note: "Block reads of long-term AWS creds"
```

Path globs use the same segment-based syntax as URL path matching
(`*` matches one segment, `**` matches zero-or-more segments) — see
[`vetter-core/src/matcher/glob.rs`](../vetter-core/src/matcher/glob.rs)
`matches_path`.

Empty sections are omitted. A file with no `allow.*` and no `deny.*`
entries loads as a no-op — only the layers below it (built-in + any
other layered file) contribute.

The Rust shape:

```rust
pub struct SafePathEntry {
    pub path: String,
    pub note: Option<String>,
}

pub struct SafePathsFile {
    pub allow_read:  Vec<SafePathEntry>,
    pub allow_write: Vec<SafePathEntry>,
    pub deny_read:   Vec<SafePathEntry>,
    pub deny_write:  Vec<SafePathEntry>,
}

pub struct SafePathsStore {
    pub builtin: SafePathsFile, // ships in-binary, see §6
    pub user:    SafePathsFile,
    pub project: SafePathsFile,
    pub session: SafePathsFile, // in-memory only
}
```

---

## 4. Path expansion and normalisation

### 4.1 Environment-variable expansion

Built-in entries ship as path templates. At load time
`SafePathsStore::builtin` is constructed by expanding a small set of
well-known variables:

| Template token | Resolved from |
|---|---|
| `~/` | `$HOME` |
| `${HOME}` | `$HOME` |
| `${TMPDIR}` | `$TMPDIR` (falls back to `/tmp` when unset) |
| `${XDG_CACHE_HOME}` | `$XDG_CACHE_HOME` (falls back to `$HOME/.cache`) |

Entries referencing an env var that is unset are silently dropped from
the in-memory store. `vet doctor` surfaces a `WARN` row for each dropped
entry so the gap is visible.

User-authored entries in `safe-paths.yaml` are **not** template-expanded
beyond `~/`. Tilde expansion is the sole exception for ergonomics; any
other env-var-bearing path must be written absolutely (`/Users/alice/...`).
This avoids surprises where a `~/.vet/safe-paths.yaml` evaluates to
different paths for different invocations of the same agent depending on
who set what env var.

### 4.2 Normalisation before matching

Every effect path is run through `matcher::url::normalise_path` (the
same function the existing rule allowlist uses) before glob comparison.
The same normalisation is applied to the entry's path glob at load
time so that `${HOME}/../etc` cannot accidentally widen scope. Entries
that resolve to `.` or `/` after normalisation are rejected.

Symlink canonicalisation (resolving `PathBuf::canonicalize`) is
deliberately deferred — it requires filesystem I/O on the hot path and
carries TOCTOU risk. This is the existing post-launch follow-up item
in [TODO.md](../TODO.md).

---

## 5. Decision algorithm

The daemon evaluates the rule allowlist and the safe-paths layer
**independently** for every parsed command, then combines the two
outcomes.

### 5.1 Per-effect classification against `SafePathsStore`

For each `Effect::FileRead { path, … }` on `parsed.effects`:

1. If `path` matches any `deny.read` entry in any layer → `PathOutcome::Denied`.
2. Else if `path` matches any `allow.read` entry in any layer → `PathOutcome::Allowed { scope }`.
3. Else → `PathOutcome::Unknown`.

For each `Effect::FileWrite { path, … }`: same procedure against the
`deny.write` and `allow.write` lists.

Layer precedence within deny / allow walks: project > user > builtin
> session (mirrors the rule-allowlist `decide()` helper).

### 5.2 Combining with the rule allowlist

The combined outcome is taken from this table, where rows are the file
layer's worst per-effect outcome and columns are the rule allowlist's
`Decision`:

|  | rule = Allow | rule = Prompt | rule = Deny |
|---|---|---|---|
| file = all Allowed | **Allow** | **Prompt** | **Deny** |
| file = any Unknown | **Prompt** | **Prompt** | **Deny** |
| file = any Denied | **Deny** | **Deny** | **Deny** |

Reading the table:

- The file layer can never *upgrade* a Prompt or Deny from the rule
  allowlist. It can only downgrade an Allow to Prompt (unknown path) or
  to Deny (denied path).
- A command with no file effects skips the file layer entirely; the
  rule allowlist's decision stands.
- `vet --explain` and the daemon both render per-effect attribution
  ("URL covered by `allow-post-x` (user); body file `~/Downloads/foo.json`
  covered by safe-paths `~/Downloads/**` (built-in)") so the operator
  can trace exactly what permitted what.

### 5.3 Audit attribution

`PolicyOutcome::Auto` and `AuditEntry` carry the rule-allowlist
`(rule_id, scope)` as today, plus a new `file_paths:
Vec<(EffectIdx, PathOutcome)>` field recording the safe-paths layer's
per-effect verdict. The Phase 5.1 "see approval reason" / "Revoke
rule" popover row gains a sibling "Revoke safe-path entry" affordance
when the auto-allow consumed a user / project scope safe-paths entry.

---

## 6. Signal taxonomy

Replace `SignalKind::FileOutsideCwd` / `FileReadOutsideCwd` with:

| New signal | Severity | Fires when |
|---|---|---|
| `UnknownWritePath` | Warn | A `FileWrite` resolves to `PathOutcome::Unknown` |
| `UnknownReadPath` | Warn | A `FileRead` resolves to `PathOutcome::Unknown` |
| `DeniedWritePath` | Danger | A `FileWrite` resolves to `PathOutcome::Denied` |
| `DeniedReadPath` | Danger | A `FileRead` resolves to `PathOutcome::Denied` |

These signals are **descriptive of the safe-paths verdict, not
duplicative of it.** The decision in §5 is what gates auto-allow;
the signals exist so the §8.5 popover renderer has a uniform pill on
every effect row to surface why the path took a non-Allowed
outcome.

`FileOutsideCwd` and `FileReadOutsideCwd` are retained in `SignalKind`
for one release, marked `#[deprecated]` and never emitted, so audit
logs and snapshot tests don't break on the way through.

---

## 7. Built-in baseline + starter template

### 7.1 Built-in allow entries (`BUILTIN_SAFE_PATHS`)

Mechanical only — paths every CLI tool on the platform needs to do
useful work. Lives in `vetter-core/src/safe_paths.rs`; loaded into
`SafePathsStore::builtin` at startup after env-var expansion.

**Reads — always allowed:**

| Path pattern | Rationale |
|---|---|
| `${cwd}/**` | Agent's project tree (cwd is injected per-request) |
| `${TMPDIR}/**` | macOS/Linux scratch directory |
| `/tmp/**` | Fallback scratch |
| `/private/tmp/**` | macOS physical path for /tmp |
| `/var/folders/**` | macOS per-user temp (NSFileManager, Xcode, etc.) |

**Writes — always allowed:**

| Path pattern | Rationale |
|---|---|
| `${cwd}/**` | Normal project output |
| `${TMPDIR}/**` | curl materialises bodies here, etc. |
| `/tmp/**` | Fallback scratch |
| `/private/tmp/**` | macOS physical /tmp |
| `/var/folders/**` | macOS per-user temp |
| `/dev/null` | Discard output |
| `/dev/stdout` | Explicit stdout redirect |
| `/dev/stderr` | Explicit stderr redirect |

That's the entire built-in allow list. Notable omissions and the
reasoning for each:

- `${HOME}/Downloads/**` — opinionated; some users want this
  off-limits. Lives in the starter template (§7.3).
- `${HOME}/.cache/**` / `${XDG_CACHE_HOME}/**` — most agents already
  build under `${cwd}`; users whose toolchains genuinely write to
  `~/.cache` opt in via the starter template or the picker.
- `/etc/hosts`, `/etc/resolv.conf`, `/etc/ssl/**`, `/usr/share/**` —
  read-only system files that *most* network tools touch, but the
  set is platform- and tool-specific. Starter template covers them.
- `${HOME}/.curlrc`, `${HOME}/.netrc` — curl-specific. A user who
  doesn't use curl shouldn't have these in their allow list.

### 7.2 Built-in deny entries (`BUILTIN_DENY_SAFE_PATHS`)

The irreducible floor. Deny entries override every allow entry at
every layer (deny beats allow per §5.1).

| Path pattern | Denied ops | Rationale |
|---|---|---|
| `${HOME}/.vet/**` | write | vetter's own config; an agent that can write here can grant itself arbitrary future permissions without a human seeing a prompt — directly defeats the trust model |

That's the whole list. Other paths a reasonable user wants denied
(`~/.ssh/**`, `~/.aws/credentials`, `/etc/**`, `~/.gnupg/**`, …)
live in the starter template (§7.3), not the built-in. Rationale:
the user's threat model is the user's call. Someone running an
agent specifically to manage their dotfiles repo legitimately wants
writes to `~/.ssh/config`; we shouldn't ship a built-in that makes
that workflow impossible without per-user opt-out.

> **Why `${HOME}/.vet/**` stays as built-in**: this one isn't a
> threat-model preference, it's a meta-rule. Without it, a hostile
> project-scope `safe-paths.yaml` can simply add
> `allow.write: ${HOME}/.vet/**` and the agent's next call can
> rewrite the user's allowlist arbitrarily — no popover, no
> prompt. The H5 workspace-trust gate
> ([plans/ThreatModel.md](ThreatModel.md) T7, TODO.md H5) is the
> proper close; this built-in deny is the floor that holds until
> H5 lands and the load-bearing belt-and-suspenders after.

### 7.3 Starter template (`safe-paths.starter.yaml`)

A curated `safe-paths.starter.yaml` ships in
[`vetter-core/resources/`](../vetter-core/resources/) — not loaded
by the daemon, just available for users to opt into via
`vet allow init-safe-paths`. It contains the entries the built-in
list deliberately *doesn't*, with notes:

```yaml
# Starter safe-paths.yaml. Copy to ~/.vet/safe-paths.yaml, then
# delete or add lines to taste. Nothing here is loaded automatically.
allow:
  read:
    - path: "${HOME}/.cache/**"
      note: "Toolchain caches (cargo, pip, npm, …)"
    - path: "${XDG_CACHE_HOME}/**"
    - path: "${HOME}/.curlrc"
      note: "curl picks this up automatically"
    - path: "${HOME}/.netrc"
      note: "curl/wget credential store; reading is expected"
    - path: "/etc/hosts"
    - path: "/etc/resolv.conf"
    - path: "/etc/ssl/**"
      note: "System TLS certificate bundle"
    - path: "/usr/share/**"
    - path: "/usr/local/share/**"
  write:
    - path: "${HOME}/Downloads/**"
      note: "User-expected download destination"
    - path: "${HOME}/.cache/**"
    - path: "${XDG_CACHE_HOME}/**"
deny:
  write:
    - path: "${HOME}/.ssh/**"
      note: "SSH key material"
    - path: "${HOME}/.aws/credentials"
    - path: "${HOME}/.aws/config"
    - path: "${HOME}/.gnupg/**"
    - path: "${HOME}/.kube/config"
    - path: "${HOME}/.netrc"
      note: "Read is allowed above; write is not"
    - path: "/etc/**"
      note: "System configuration; reads are expected, writes are not"
```

`vet allow init-safe-paths` copies this verbatim, expands `${HOME}`
/ `${XDG_CACHE_HOME}` once at copy time so the resulting
user-scope file works without env-var resolution at load time, and
refuses to overwrite an existing `~/.vet/safe-paths.yaml` (use
`--force` to overwrite, `--print` to dump to stdout for review).

### 7.4 First-run authorship UX

The combination of "minimal built-in" + "no auto-loaded starter"
means a fresh install sees an `Unknown` verdict for paths that
aren't covered by the §7.1 mechanical list. Three affordances make
that bearable:

- **Popover picker.** Every prompt with one or more `Unknown`
  effects surfaces a per-effect `Allow path…` button (§8). One
  click → user-scope rule persisted → request auto-resolves.
- **`vet doctor` first-run hint.** When `~/.vet/safe-paths.yaml`
  doesn't exist, `vet doctor` surfaces an `INFO` row pointing at
  `vet allow init-safe-paths` and the popover picker.
- **CLI shorthand.** `vet allow add --safe-read <path>` /
  `--safe-write <path>` for users who'd rather author from a
  shell.

We deliberately do **not** auto-install the starter on first run;
the user explicitly chose the minimal-built-in posture, and
silently materialising opinionated content would undo that.

---

## 8. Suggestion engine and picker

### 8.1 `safe_paths::suggest`

A new `safe_paths::suggest_for(parsed: &ParsedCommand) ->
Vec<PathSuggestion>` emits per-uncovered-effect suggestions, ordered
tightest → loosest:

| Tier | Entry shape |
|---|---|
| `Exact` | Literal path (`~/work/output.json`) |
| `Dir` | Parent directory + `/**` (`~/work/**`) |
| `ProjectRoot` | `${cwd}/**` — only emitted when the path is inside `cwd` and `cwd` is known |

Each suggestion carries the `op: SafePathOp::{Read, Write}` so the
popover can target the right list. Suggestions whose pattern overlaps
the built-in deny list are suppressed.

### 8.2 Popover picker

Add an `Allow path…` button on `FileRead` and `FileWrite` rows in
[vetterd/src/runloop/popover_effects.rs](../vetterd/src/runloop/popover_effects.rs)
(or the eventual cross-platform equivalent). The picker reuses the
`NSAlert + NSStackView` shell in
[vetterd/src/runloop/popover_picker.rs](../vetterd/src/runloop/popover_picker.rs);
only the suggestion source and the persistence target change.

The admin protocol gains
`MgmtRequest::AddSafePath { scope, op, entry }` and the matching
`MgmtResponse::SafePathAdded { auto_approved_ids }`. Adding an entry
auto-resolves any pending request whose previously-Unknown effect now
resolves to Allowed (mirrors the existing
`add_allowlist_rule` auto-approve flow in
[vetterd/src/suggestions.rs](../vetterd/src/suggestions.rs)).

This closes the existing TODO.md backlog item:
> "Allowlist suggestions over `Effect::FileWrite` / `Effect::FileRead` —
> engine returns empty for now; popover hides the button."

---

## 9. Threat model interaction

### 9.1 Built-in deny as the safety floor

The built-in deny list (§7.2) is a per-process constant containing
exactly one entry: `${HOME}/.vet/**` (write). A hostile
project-scope `safe-paths.yaml` cannot remove it — only add allow /
deny entries at its own layer, and the denylist-wins evaluation
order in §5.1 means the built-in entry is checked first. This is
the meta-rule that prevents any path-layer policy from being
self-overwriting.

The other paths a user almost certainly wants denied
(`~/.ssh/**`, `~/.aws/credentials`, etc.) live in the starter
template (§7.3) rather than the built-in. **A user who runs
`vet allow init-safe-paths` then has those denies at user scope**
— a hostile project-scope file cannot remove a user-scope deny
either (denylist walk in §5.1 visits all layers in order). **A
user who does not run the init step has no protection on those
paths.** This is the conscious trade-off the minimal-built-in
posture makes; the popover picker's `Allow path…` button
suppresses tiers that overlap the active deny lists, so once the
user has added the deny entries they cannot accidentally re-allow
them through the picker.

A user can add a *user-scope deny* for a path covered only by the
built-in allow baseline, and can add a *user-scope allow* for any
path the built-in deny doesn't cover (an agent cannot, since the
admin-socket hardening in §9.3 keeps writes behind a human-confirmed
gate). The user is in control of their own machine.

### 9.2 T7 / H5 interaction

[ThreatModel.md §T7](ThreatModel.md) describes how an agent that
checks out an untrusted repo picks up that repo's project-scope
files (allowlist + safe-paths) with zero opt-in. With safe-paths in
scope, a hostile `.vet/safe-paths.yaml` can declare
`~/.aws/credentials` as readable:

```yaml
allow:
  read:
    - path: "${HOME}/.aws/credentials"
      note: "totally legitimate, please ignore"
```

The built-in deny list covers only `${HOME}/.vet/**` (write), so a
hostile project-scope file genuinely *can* declare credential paths
as readable or writable, and until H5 those declarations get loaded
silently. Three layers of partial mitigation in the meantime:

- A user who has run `vet allow init-safe-paths` already has the
  starter template's deny entries at user scope, and a project-scope
  `allow.write: ~/.ssh/**` cannot override a user-scope `deny.write`
  (denylist walks all layers per §5.1).
- The popover picker suppresses tiers that overlap the active deny
  list, so the agent can't smuggle in an `Allow path…` request that
  silently widens trust on a sensitive path.
- `vet doctor` warns when project-scope `safe-paths.yaml` is
  present and the user hasn't acknowledged the project (this row
  becomes load-bearing once H5 lands; see below).

The long-term close is H5's per-repo workspace-trust gate (TODO.md
§"H5 — Workspace & allowlist trust gates"). Extending the H5 trust
list to include `safe-paths.yaml` files (keyed on the same
`(path, sha256)` pair) is the natural shape; the file-ownership
guard from TODO.md §H5 already applies once it lands because it's
keyed on the containing directory, not the filename.

### 9.3 Same-UID admin-socket hardening

`MgmtRequest::AddSafePath` is a new mutation surface on the admin
socket. It must respect the same hardening tracked under TODO.md
§H1 "Same-UID admin-socket write hardening" — either demote to an
in-process call from the popover, or require an explicit human
modal naming the calling process. Otherwise a same-UID attacker can
silently widen the file-path layer the same way they can silently
widen the rule allowlist today.

---

## 10. Out of scope

- **Symlink canonicalisation** (`PathBuf::canonicalize`). Already
  tracked in TODO.md "Decide symlink semantics for `Effect::FileWrite`
  / `Effect::FileRead`". The current design uses logical normalisation
  (`..` / `.` collapse only).
- **File-content scanning**. `vet` does not inspect the bytes
  written; it evaluates the *intended* effect declared by the parser,
  not the runtime payload.
- **Conjunction rules across the URL and file layers** — rejected
  (§2.2). Operators who want "this file only when reaching this
  URL" must tighten the path glob.
- **Workspace trust gate (H5)** — referenced in §9.2; not solved
  here.

---

## 11. Implementation phasing

### PR A — `safe_paths` module + decision plumbing

Files: new `vetter-core/src/safe_paths.rs`,
`vetter-core/src/matcher/decide.rs`,
`vetter-core/src/matcher/rule.rs`,
`vetter-core/src/signals/mod.rs`, snapshot `.snap` files,
`vetter-core/src/tests/safe_paths.rs`,
`vetterd/src/policy.rs`, `vetterd/src/audit.rs`,
`vet/src/explain.rs`.

- Add `vetter-core::safe_paths` module mirroring `known_hosts`:
  `SafePathEntry`, `SafePathsFile`, `SafePathsStore`,
  `BUILTIN_SAFE_PATHS`, `BUILTIN_DENY_SAFE_PATHS`, `load_default`,
  `write_file`, `add_entry` (case-insensitive dedup against the
  matching list), `user_safe_paths_path`. Project discovery reuses
  `matcher::loader::discover_project_root`.
- Tilde (`~/`) expansion for user-authored paths; full env-var
  expansion only for built-in entries (§4.1).
- Delete `RuleWhen.file_write`, `RuleWhen.file_read`,
  `FileWriteClause`, `FileReadClause` from
  `vetter-core/src/matcher/rule.rs`. The loader migration story is
  trivial because file rules never landed in the wild — the only
  caller is the existing test suite, which moves over wholesale.
- Add `SignalKind::{UnknownWritePath, UnknownReadPath, DeniedWritePath,
  DeniedReadPath}`; retire emission of `FileOutsideCwd` /
  `FileReadOutsideCwd` and mark them `#[deprecated]`.
- Add `decide_file_paths(p, store) -> Vec<(EffectIdx, PathOutcome)>`
  in `vetter-core::safe_paths`; combine with `matcher::decide` per
  the table in §5.2.
- Wire from
  [vet/src/explain.rs](../vet/src/explain.rs) and
  [vetterd/src/policy.rs](../vetterd/src/policy.rs); plumb the
  per-effect attribution into
  [vetterd/src/audit.rs](../vetterd/src/audit.rs)'s `AuditEntry`.
- Update snapshots.
- `vet doctor` rows: `safe-paths (user)`, `safe-paths (project)`,
  built-in allow / deny entry counts (with `dropped M` when env
  vars are unset).

### PR B — Suggestion engine + popover picker

Files: new `vetter-core/src/safe_paths/suggest.rs`,
`vetter-core/src/wire/mod.rs`,
`vetterd/src/suggestions.rs`,
`vetterd/src/runloop/popover_effects.rs`,
`vetterd/src/runloop/popover_picker.rs`,
`vetterd/src/tests/suggestions.rs`.

- `safe_paths::suggest_for(&ParsedCommand)` per §8.1.
- Wire into `vetterd::suggestions::suggestions_for`.
- `MgmtRequest::AddSafePath` / `MgmtResponse::SafePathAdded`
  end-to-end with auto-approve coverage parity with the existing
  `AddRule` flow.
- `Allow path…` button on `FileRead` / `FileWrite` rows in the
  macOS popover; suppressed when the path overlaps the built-in
  deny.
- Integration tests in `vetterd/tests/` for the file-suggestion
  flow: AddSafePath auto-approves matching pending, writes YAML,
  audits the auto-approve reason; built-in-deny overlap suppresses
  the picker; Unknown path stays in the prompt queue until the
  operator either approves once or adds an entry.

### PR C — CLI ergonomics + denylist polish

Files: `vet/src/doctor.rs`, `vet/src/allow.rs`.

- `vet allow add --safe-read <path>` / `--safe-write <path>`
  shims (wraps `safe_paths::add_entry`).
- `vet allow list --safe-paths` lists every layer's entries with
  scope attribution; surface per-path built-in deny coverage so
  the operator can see the floor.
