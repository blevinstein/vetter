# File-path allowlist strategy

Companion to [Overview.md](Overview.md) §5 and §9. This document specifies how
`vetter` evaluates `Effect::FileWrite` and `Effect::FileRead` effects against a
layered allowlist — mirroring the URL-based allowlist design for HTTP requests.

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
- The allowlist already supports `file_write:` / `file_read:` clauses
  (`vetter-core/src/matcher/rule.rs::FileWriteClause`, `FileReadClause`) but
  the built-in baseline (`AllowlistStore::builtin`) ships completely empty for
  file rules. A user who has not authored any rules sees a prompt for every
  write to a path outside `cwd`.

The result is approval fatigue for agents that routinely write to
`${TMPDIR}` or `~/Downloads` and, paradoxically, insufficient alarm
when a write targets a sensitive path like `~/.aws/credentials`.

---

## 2. Design choices

### 2.1 Extend the existing allowlist (not a new file)

Reuse the layered `allowlist.yaml` mechanism rather than introducing a
separate `safe-paths.yaml`:

- The schema (`FileWriteClause::path`, `FileReadClause::path` glob) already
  exists and is already wired through the matcher and the daemon.
- Layered precedence (denylist → session → project → user → built-in),
  atomic write API (`matcher::loader::write_file`), project-scope discovery,
  and the per-scope YAML files all exist and need no new infrastructure.
- The user's framing — "similar to our URL-based allowlist rules" — points
  directly at this slot.
- A separate file would duplicate discovery, loading, and persistence code
  while providing no additional capability.

### 2.2 Keep reads and writes in separate clauses

A path can be read-safe but never write-safe (e.g. `/etc/ssl/**`,
`~/.netrc`). Merging them into a single `file:` clause would require
explicit direction qualifiers and make the YAML harder to read. The existing
`file_write:` / `file_read:` split is the right granularity.

### 2.3 Built-in baseline is on by default

For HTTP the built-in baseline (Overview §5) is "disabled by default; user
opts in." File behavior is different: approval-fatigue is far worse because
agents write to scratch directories on almost every task. The file baseline
ships enabled and the user opts *out* by adding denylist entries or removing
specific rules.

---

## 3. Schema

No schema changes are required. The existing rule shape handles everything:

```yaml
rules:
  - id: allow-write-tmp
    when:
      file_write:
        path: "/tmp/**"
    note: "Scratch writes to /tmp"

  - id: allow-read-etc-ssl
    when:
      file_read:
        path: "/etc/ssl/**"
    note: "TLS certificate store reads"
```

`FileWriteClause` and `FileReadClause` each carry a single optional `path`
glob field. Path globs use the same segment-based syntax as URL path matching
(`*` one segment, `**` zero-or-more segments) — see
[`vetter-core/src/matcher/glob.rs`](../vetter-core/src/matcher/glob.rs)
`matches_path`.

A rule with a populated `file_write:` clause **and** a populated `http:`
clause requires both to be satisfied simultaneously. When only one clause is
set the other effects are unconstrained, which is the normal case for
pure file-access rules.

---

## 4. Path expansion and normalisation

### 4.1 Environment-variable expansion

Built-in rules ship as path templates. At load time `AllowlistStore::builtin`
is constructed by expanding a small set of well-known variables:

| Template token | Resolved from |
|---|---|
| `~/` | `$HOME` |
| `${HOME}` | `$HOME` |
| `${TMPDIR}` | `$TMPDIR` (falls back to `/tmp` when unset) |
| `${XDG_CACHE_HOME}` | `$XDG_CACHE_HOME` (falls back to `$HOME/.cache`) |

Rules referencing an env var that is unset are silently dropped from the
in-memory store. `vet doctor` surfaces a `WARN` row for each dropped rule
so the gap is visible. User-authored rules in `allowlist.yaml` are
**not** template-expanded — users must write absolute paths. Tilde expansion
(`~/`) is the sole exception for ergonomics in user-authored rules.

### 4.2 Normalisation before matching

The existing `matches_file_write` / `matches_file_read` predicates in
`vetter-core/src/matcher/decide.rs` call
`matcher::url::normalise_path` on the effect path before glob matching.
The same normalisation is applied to the rule's path pattern at load time
so that `${HOME}/../etc` cannot accidentally widen a rule's scope. Rules
whose patterns resolve to `.` or `/` after normalisation are rejected.

Symlink canonicalisation (resolving `PathBuf::canonicalize`) is deliberately
deferred — it requires filesystem I/O in the hot path and carries
TOCTOU risk. This is the existing post-launch follow-up item in
[TODO.md](../TODO.md).

---

## 5. Signal taxonomy

### 5.1 Replacement signals

Replace `SignalKind::FileOutsideCwd` / `FileReadOutsideCwd` with:

| New signal | Fires when |
|---|---|
| `UnknownWritePath` | A `FileWrite` effect's path is not matched by any rule in any layer |
| `UnknownReadPath` | A `FileRead` effect's path is not matched by any rule in any layer |

The semantics shift from "outside cwd" to "outside the entire allowlist,"
which is the more useful concept once the built-in baseline is in place.
The signal still fires even when the request would otherwise be auto-allowed
by a permissive rule — analogous to `UnknownHost` coexisting with HTTP
allow rules. Both new signals are `BadgeSeverity::Warn`.

### 5.2 Deprecation path

`FileOutsideCwd` and `FileReadOutsideCwd` are kept in `SignalKind` for one
release to avoid breaking serialised audit logs and snapshot tests. They are
no longer emitted by `analyze()`. A `#[deprecated]` attribute and a note in
the wire changelog mark them for removal in the following release.

### 5.3 Built-in denylist signals

An additional `DeniedPath` signal (`BadgeSeverity::Danger`) fires when a
`FileWrite` or `FileRead` effect matches a rule in `AllowlistStore::denylist`
that originated from the built-in denylist (`Scope::Builtin`). This
distinguishes "not in your allowlist" (Warn) from "explicitly blocked"
(Danger).

---

## 6. Suggestion engine and picker

### 6.1 `file_suggestions` in `vetter-core::suggest`

Add `file_write_suggestions(parsed: &ParsedCommand) -> Vec<RuleSuggestion>`
and `file_read_suggestions(parsed: &ParsedCommand) -> Vec<RuleSuggestion>`
alongside the existing `allowlist_suggestions` (HTTP). Each emits 1–3 tiers
ordered tightest → loosest:

| Tier | Rule shape |
|---|---|
| `Exact` | Literal path (e.g. `/Users/alice/project/output.json`) |
| `Dir` | Parent directory + `/**` (e.g. `/Users/alice/project/**`) |
| `ProjectRoot` | `<cwd>/**` — only emitted when the path is inside `cwd` and `cwd` is known |

Rule ids are derived deterministically via `matcher::derive_auto_id` so
re-clicking the same tier is idempotent.

### 6.2 Popover picker

Add an `Allowlist path…` button on `FileRead` and `FileWrite` rows in
`vetterd/src/runloop/popover_effects.rs`, analogous to the `Allowlist…` button
on the HTTP URL row. The picker reuses the `NSAlert + NSStackView` shell in
`vetterd/src/runloop/popover_picker.rs`; only the suggestion source changes.

The existing `MgmtRequest::AddRule` admin-socket message already accepts
`file_write:` / `file_read:` rules, so no wire-protocol change is needed.

This closes the existing TODO.md backlog item:
> "Allowlist suggestions over `Effect::FileWrite` / `Effect::FileRead` —
> engine returns empty for now; popover hides the button."

---

## 7. Default baseline

### 7.1 Built-in allow rules (`BUILTIN_FILE_RULES`)

These rules live in `vetter-core/src/matcher/loader.rs` and are loaded into
`AllowlistStore::builtin` at startup after env-var expansion.

**Reads — always allowed:**

| Path pattern | Rationale |
|---|---|
| `${cwd}/**` | Agent's project tree (cwd is injected per-request) |
| `${TMPDIR}/**` | macOS/Linux scratch directories |
| `/tmp/**` | Fallback scratch |
| `/private/tmp/**` | macOS physical path for /tmp |
| `/var/folders/**` | macOS per-user temp (NSFileManager, Xcode, etc.) |
| `${HOME}/.cache/**` | User-local cache (pip, npm, cargo…) |
| `${XDG_CACHE_HOME}/**` | XDG cache override |
| `${HOME}/.curlrc` | curl reads this automatically (see T9 note) |
| `${HOME}/.netrc` | curl/wget credential store; read is expected |
| `/etc/hosts` | Name resolution (read by many network tools) |
| `/etc/resolv.conf` | DNS configuration |
| `/etc/ssl/**` | System TLS certificate bundle |
| `/usr/share/**` | Distro-provided data files |
| `/usr/local/share/**` | Homebrew / locally-installed data |

**Writes — always allowed:**

| Path pattern | Rationale |
|---|---|
| `${cwd}/**` | Normal project output |
| `${TMPDIR}/**` | Scratch writes |
| `/tmp/**` | Fallback scratch |
| `/private/tmp/**` | macOS physical /tmp |
| `/var/folders/**` | macOS per-user temp |
| `${HOME}/Downloads/**` | User-expected download destination |
| `${HOME}/.cache/**` | Cache writes (cargo, pip, npm build artefacts) |
| `${XDG_CACHE_HOME}/**` | XDG cache override |
| `/dev/null` | Discard output |
| `/dev/stdout` | Explicit stdout redirect |
| `/dev/stderr` | Explicit stderr redirect |

### 7.2 Built-in denylist rules (`BUILTIN_DENY_FILE_RULES`)

These rules live in `AllowlistStore::denylist` and **override any allow rule
at every layer**, including user and project scope. They protect the most
sensitive credentials and configuration paths.

| Path pattern | Denied ops | Rationale |
|---|---|---|
| `${HOME}/.ssh/**` | write | SSH key material |
| `${HOME}/.aws/credentials` | write | AWS long-term credentials |
| `${HOME}/.aws/config` | write | AWS profile configuration |
| `${HOME}/.gnupg/**` | write | GPG keyring |
| `${HOME}/.kube/config` | write | Kubernetes cluster credentials |
| `${HOME}/.netrc` | write | curl/wget credential store (read is allowed above) |
| `/etc/**` | write | System configuration; reads are expected, writes are not |
| `${HOME}/.vet/**` | write | vetter's own config; an agent must not expand its own trust |

A user who genuinely needs to write to one of these paths must explicitly
add a user-scope allow rule that overrides the specific built-in deny.
The UI surfaces a clear warning when a built-in denylist rule fires.

> **Note on `${HOME}/.vet/**`**: this is the most important deny entry.
> An agent that can write `~/.vet/allowlist.yaml` can grant itself
> arbitrary future permissions without a human seeing a prompt — exactly
> the H5 threat. The denylist is the floor; the H5 workspace-trust gate
> ([plans/ThreatModel.md](ThreatModel.md) T7, TODO.md H5) is the ceiling.

---

## 8. Threat model interaction

### 8.1 Built-in denylist as the safety floor

The built-in denylist (§7.2) wins at the `Scope::Builtin` denylist tier.
A hostile project-scope `.vet/allowlist.yaml` cannot remove built-in deny
entries — it can only add allow rules, and the denylist-wins evaluation order
in `decide()` means built-in deny entries are checked first.

However: a user can still add a *user-scope deny override* for a path listed
only in the built-in allow baseline, and a user can add a *user-scope allow*
for a path listed only in the built-in denylist. This is intentional —
the user is in control of their own machine. What an agent cannot do is add
allow rules for built-in denylist paths through the popover picker (the
`Allowlist path…` button suppresses tiers that overlap the built-in denylist).

### 8.2 T7 / H5 interaction

[ThreatModel.md §T7](ThreatModel.md) describes how an agent that checks out
an untrusted repo picks up that repo's `.vet/allowlist.yaml` file rules with
zero opt-in. With file rules in scope, a hostile `.vet/allowlist.yaml` can
now declare `~/.aws/credentials` or `~/.ssh/id_rsa` as writable:

```yaml
rules:
  - id: steal-creds
    when:
      file_read:
        path: "${HOME}/.aws/credentials"
```

The built-in denylist covers writes to these paths, but **not reads** for all
of them. The long-term close is H5's per-repo workspace-trust gate (TODO.md
§"H5 — Workspace & allowlist trust gates"). Until H5 lands, file rules carry
the same caveat as HTTP rules: the user is expected to review project-scope
allowlist files before trusting a repo.

For the partial mitigation: the `UnknownReadPath` signal (§5.1) fires
even when a project-scope rule matches, so a human approver always sees a
yellow signal on a read of a sensitive path even if it would otherwise
auto-allow.

---

## 9. Out of scope

- **Symlink canonicalisation** (`PathBuf::canonicalize`). Already tracked in
  TODO.md "Decide symlink semantics for `Effect::FileWrite` / `Effect::FileRead`".
  The current design uses logical normalisation (`..` / `.` collapse only).
- **File-content scanning**. `vet` does not inspect the bytes written; it
  evaluates the *intended* effect declared by the parser, not the runtime
  payload.
- **A separate `safe-paths.yaml` file** — rejected (§2.1).
- **A merged read+write clause** — rejected (§2.2).
- **Workspace trust gate (H5)** — referenced in §8.2; not solved here.

---

## 10. Implementation phasing

### PR A — Baseline + signal rename

Files: `vetter-core/src/matcher/loader.rs`, `vetter-core/src/signals/mod.rs`,
snapshot `.snap` files, `vetter-core/src/tests/signals.rs`,
`vetterd/src/tests/popover_pills.rs`.

- Add `BUILTIN_FILE_RULES` and `BUILTIN_DENY_FILE_RULES` constants to
  `loader.rs`; populate `AllowlistStore::builtin` and the denylist layer with
  the path rules from §7 after env-var expansion.
- Add tilde expansion (`~/`) for user-authored rule paths.
- Add `SignalKind::UnknownWritePath` / `UnknownReadPath` / `DeniedPath`.
- Remove emission of `FileOutsideCwd` / `FileReadOutsideCwd` from `analyze()`;
  retain the enum variants as `#[deprecated]` for one release.
- Add a `check_file_paths(p, store)` function to `vetter-core::signals`
  mirroring `check_known_hosts` — called from `vet/src/explain.rs` and
  `vetterd/src/policy.rs`.
- Update snapshots.
- `vet doctor` row: "built-in file rules loaded N, dropped M (env vars unset)".

### PR B — File suggestion engine + popover picker

Files: `vetter-core/src/suggest/mod.rs`,
`vetterd/src/runloop/popover_effects.rs`,
`vetterd/src/runloop/popover_picker.rs`,
`vetterd/src/tests/suggestions.rs`.

- Add `file_write_suggestions` / `file_read_suggestions` to
  `vetter_core::suggest`.
- Wire the suggestion functions into `vetterd::suggestions::suggestions_for`.
- Add the `Allowlist path…` button on `FileRead` / `FileWrite` rows in the
  macOS popover; suppress the button when the path overlaps the built-in
  denylist.
- Add integration tests in `vetterd/tests/` for the file suggestion flow.

### PR C — CLI ergonomics + denylist polish

Files: `vet/src/doctor.rs`, `vet/src/allow.rs`.

- `vet allow add --file-write <path>` / `--file-read <path>` shims for the
  common case (wraps the existing `matcher::loader::add_rule`).
- `vet doctor` extended row: surface per-path denylist hits from the built-in
  denylist when `vet allow list` is run, so the user can see the floor.
