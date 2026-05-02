# Threat Model

This document expands [Overview.md](Overview.md) §6 into a concrete
actor / asset / boundary model with per-threat mitigations. It is the
source of truth for "what is `vetter` actually defending, and against
whom." Keep it in sync with the code — if you change a trust boundary,
update this file in the same PR.

Status legend: `[done]` shipped · `[planned]` on the roadmap with a
TODO entry · `[open]` undecided · `[accepted]` known residual risk we
have decided not to address.

---

## 1. Scope

`vetter` interposes between an LLM agent and the user's shell. The
agent emits a command (`vet curl …`), `vet` parses it, the daemon
`vetterd` decides allow/deny against an allowlist, and `vet` either
`execvp`s or refuses.

In scope:
- The wire from `vet` to `vetterd` (Unix socket, JSON frames).
- The on-disk allowlist files at user and project scope.
- The audit log.
- The render path (what the user sees on stderr) and the approver UI
  (Phase 4).
- The parser layer's correctness as a *security control* — a parser
  that under-counts effects produces an incorrect decision.

Out of scope (see §6): adversarial agents that bypass `vet` entirely,
full sandboxing, network MITM (we do not inspect TLS), supply-chain
compromise of `curl` / `vetterd` / the OS.

## 2. Actors

| Id | Actor | Trust |
|----|-------|-------|
| H | Human user at the workstation | Fully trusted |
| A-aligned | Aligned-but-fallible LLM agent | Honest but mistake-prone — primary protection target |
| A-adv | Adversarial code in the agent context (prompt injection, malicious tool output that gets executed) | Hostile, same UID as H |
| L | Other local user on the same machine | Hostile, different UID |
| N | Remote network attacker | Hostile, no host access |
| H' | Future operator reading the audit log forensically | Trusted |

## 3. Assets

| Id | Asset | Why it matters |
|----|-------|----------------|
| A1 | Allow / deny decisions at the moment of execution | Core control surface |
| A2 | Allowlist files (`~/.config/vet/allowlist.yaml`, `<repo>/.vet/allowlist.yaml`) | Decisions derive from these |
| A3 | Audit log | Forensic value; tampering breaks post-hoc review |
| A4 | The IPC channel between `vet` and `vetterd` | A1 derives from this |
| A5 | The `vetterd` process identity | A1 derives from believing we are talking to the real daemon |
| A6 | The `vet` process identity | Phase 4 session caching may key on it |
| A7 | Stdin payloads and request bodies (e.g. `curl -d @-`) | May contain credentials |
| A8 | The approver UI (Phase 4) | Coalescing/reordering bugs here are a social-engineering surface |

## 4. Trust boundaries

Listed in priority order — top items get attention first because they
sit closest to the agent.

- **B1. Harness ↔ vet.** Harness allowlists `vet *` and denies
  network-capable commands directly.
- **B2. vet ↔ vetterd.** Process boundary inside one UID. Trust:
  kernel-enforced UID isolation plus peer-credential authentication.
  *This is the focus of the near-term roadmap (§7).*
- **B3. vetterd ↔ allowlist files.** File-system boundary. Trust:
  ownership, mode bits, atomic writes.
- **B4. vetterd ↔ audit log.** File-system boundary. Trust:
  exclusive ownership of the log file and parent dir.
- **B5. vetterd ↔ approver UI.** Phase 4. Trust: macOS code signing,
  `LSUIElement` bundle, separate window owned by the human.
- **B6. vet ↔ wrapped binary.** After `execvp`, `vet` is gone — there
  is no continuing enforcement. Trust: the wrapped binary does what
  its argv said.

---

## 5. Threats and mitigations

Threat IDs name the boundary they cross.

### B1. Harness ↔ vet

**T-B1.1 — Agent issues `curl …` directly without going through `vet`.**
*Actor*: A-aligned (mistake) or A-adv. *Mitigation*: harness must
allowlist `vet *` and deny network-capable shells/clients.
Documentation only; not enforced by `vetter`. *Status*: `[accepted]`,
explicit per Overview.md §6. Track as documentation work — ship a
recommended harness config alongside the project.

**T-B1.2 — Shell expansion produces argv the user did not authorise
(`vet curl $URL` where `$URL` came from a model output).**
This is what `vetter` is *for* — the parser sees the post-expansion
argv and the policy decides. *Status*: `[done]` (core feature).

### B2. vet ↔ vetterd

**T-B2.1 — Cross-UID hijack of the socket path.**
*Actor*: L. *Vector*: `$TMPDIR/vetter.sock` lives in `/tmp` on Linux,
world-traversable. *Mitigation*: move the socket under
`$XDG_RUNTIME_DIR` (Linux) / `~/Library/Application Support/vetter/run/`
(macOS); validate every ancestor is owned by the current UID with no
group/other write bits; canonicalise to refuse symlinks escaping
`$HOME`. Socket mode `0o600`, parent dir `0o700`. *Status*: `[planned]`.

**T-B2.2 — Daemon impersonation (same-UID attacker binds before
`vetterd`).**
*Actor*: A-adv. *Vector*: a process running as the user binds the
socket path first; `vet` connects, sends the full `VetRequest`,
receives a forged `Allow`, then `execvp`s the unverified command.
*Impact*: complete bypass of allowlist + audit log + UI.
*Mitigation*, layered:
1. **Client-side peer-cred check.** `vet` calls `getpeereid()` /
   `SO_PEERCRED` on the connected stream and aborts on UID mismatch.
2. **PID attestation.** `vetterd` holds an `flock` on
   `<rundir>/vetterd.pid`; `vet` reads the locking PID and asserts it
   equals the peer PID from the socket.
3. **Binary attestation (Phase 4, macOS).** `vet` resolves the peer
   PID's executable and verifies its codesign team identifier.

*Status*: `[planned]`. PID attestation depends on `flock` infrastructure
that does not exist yet.

**T-B2.3 — `vet` impersonation (forged client bypasses the parser).**
*Actor*: A-adv. *Vector*: a same-UID process opens the daemon socket
and sends a `VetRequest` whose `argv` describes a benign command but
whose `parsed.effects` describe a different one. The daemon today
evaluates against `req.parsed`, not `req.argv`
(vetterd/src/lib.rs:142). *Impact*: allowlist bypass; audit log records
the lie. *Mitigation*: the daemon re-parses `argv` itself and ignores
(or hint-only-treats) the client's `parsed`. Stdin is forwarded as
bytes (≤ 1 MiB cap already in place) so the daemon can re-hash and
re-parse. Wire-protocol break — bumps `PROTOCOL_VERSION` to 2.
*Status*: `[planned]` (Phase 3c).

**T-B2.4 — Peer-credential check missing on the accept side.**
*Actor*: L. *Vector*: defence in depth on top of T-B2.1; if the path
hardening is bypassed (mis-set `$XDG_RUNTIME_DIR`, host migration,
mount-over), other-UID processes can still connect.
*Mitigation*: `getpeereid()` / `SO_PEERCRED` on `vetterd`'s accepted
streams; reject UID mismatch with a logged-but-silent close.
*Status*: `[planned]`.

**T-B2.5 — Resource exhaustion against the daemon.**
*Actor*: A-adv (also a DoS surface for A-aligned bugs). *Vector*: open
many connections, stall mid-frame; `vetterd` spawns one OS thread per
connection (vetterd/src/lib.rs:119) with no read deadline and no
concurrency cap. *Impact*: vetterd unresponsive → `vet` fails closed →
user cannot run any commands. *Mitigation*: 5 s read deadline on the
request frame; cap inflight workers (default 16, configurable via
`VETTERD_MAX_INFLIGHT`); accept-and-immediately-close above the cap.
*Status*: `[planned]`.

**T-B2.6 — Replay or cross-request confusion.**
*Mitigation*: `VetDecision.id` echoes `VetRequest.id`; `read_decision`
returns `WireError::IdMismatch` on divergence (vetter-core/src/wire/mod.rs).
One request per connection. *Status*: `[done]`.

**T-B2.7 — Wire-version downgrade.**
*Vector*: a future `v=2` daemon is rolled back to a `v=1` build
mid-deploy, or vice versa. *Mitigation*: hard `VersionMismatch` today —
fail closed, no negotiation. Acceptable while there is one version.
When v2 ships, decide between strict equality + documented upgrade
ordering and a hello-frame negotiation. *Status*: `[done]` for v1;
revisit at v2.

### B3. vetterd ↔ allowlist files

**T-B3.1 — Symlink attack on `~/.config/vet/allowlist.yaml`.**
*Actor*: L (or A-adv with home-dir write). *Mitigation*: open with
`O_NOFOLLOW` on the final component; refuse if the file is not owned
by the current UID or has group/other write bits. *Status*: `[planned]`.

**T-B3.2 — Crash mid-write corrupts the file.**
*Mitigation*: temp-file + rename atomic write. *Status*: `[planned]`
(Phase 3b: "Atomic allowlist writes" in TODO.md).

**T-B3.3 — TOCTOU between rule load and rule match.**
*Vector*: user edits the allowlist while a request is in flight; the
daemon evaluates against a stale copy. *Mitigation*: either inotify /
FSEvents-driven invalidation, or "load on every request" (cheap; YAML
files are small). *Status*: `[open]`. Decide before Phase 4.

### B4. vetterd ↔ audit log

**T-B4.1 — Other-UID reads sensitive request bodies / headers from
the log.** *Mitigation*: explicit `chmod 0o600` on creation; `0o700`
on parent dir; refuse to start if an existing log has looser perms.
*Status*: `[planned]`.

**T-B4.2 — Log grows unbounded and fills the disk.**
*Mitigation*: size- or time-based rotation; document a default
(starting point: 64 MiB × 4 generations). *Status*: `[planned]`.

**T-B4.3 — Tampering by a same-UID attacker.**
*Status*: `[accepted]`. Same-UID write means the attacker can rewrite
history; cryptographic chaining is overkill for MVP. Future option:
append-only flag on macOS (`chflags uchg`).

### B5. vetterd ↔ approver UI

Deferred to Phase 4. Notable threats to surface in the Phase 4 plan:

- Notification spoofing by another app on macOS (mitigated by code
  signing).
- Click-jacking of the Approve button via timing.
- Coalescing collapsing distinct requests into one Approve action.

### B6. vet ↔ wrapped binary

**T-B6.1 — Wrapped binary differs from what the parser saw (TOCTOU on
the binary itself).** *Vector*: between `parsers::dispatch` and
`Command::exec`, an attacker replaces `/usr/bin/curl`.
*Status*: `[accepted]` for MVP. A same-UID attacker has many easier
vectors.

**T-B6.2 — argv0 spoofing.**
*Vector*: `ln /bin/bash /tmp/curl && vet /tmp/curl …` — basename
dispatch routes to the curl parser, but exec runs bash.
*Mitigation*: dispatcher resolves argv[0] to a real path and refuses
unknown inodes (or matches against an expected list of curl install
paths). *Status*: `[planned]` (cross-cutting hardening list).

---

## 6. Cross-cutting non-goals

Reaffirmed from Overview.md §6:

1. We do not protect against an adversarial agent that bypasses `vet`
   entirely. The harness is the line of defence; we publish a
   recommended config but cannot enforce it.
2. We are not a sandbox. No syscall filtering, no namespaces, no
   seccomp.
3. We do not inspect TLS payloads — we evaluate intent before the
   call leaves the box.
4. We do not centralise audit logs.
5. We do not protect against compromise of the host OS, the wrapped
   binary, or `vetterd` itself.

---

## 7. IPC roadmap (B2)

The IPC hardening lands in three PRs. Each is independently shippable;
the value scales with how many you ship.

| PR | Threats closed | Surface |
|----|----------------|---------|
| 1 — Path + peer-cred + deadlines | T-B2.1, T-B2.4, T-B2.5; partial T-B2.2 | `vetter-core/src/paths.rs` (new), `vetterd/src/{paths,socket,lib}.rs`, `vet/src/wrap.rs`, `vetter-core/src/wire/mod.rs` (`WireError::PeerAuth`) |
| 2 — Daemon re-parse | T-B2.3 | `vetter-core/src/wire/mod.rs` (drop or demote `parsed`, bump `PROTOCOL_VERSION` to 2, forward stdin), `vetterd/src/{lib,policy}.rs` (re-parse), `vet/src/wrap.rs` |
| 3 — PID attestation + audit fields | Remainder of T-B2.2 | `vetterd/src/lib.rs` (flock pid file), `vet/src/wrap.rs` (PID attestation), `vetterd/src/audit.rs` (`peer_pid` / `peer_uid` / `peer_exe`) |

Phase 4 layers macOS codesign verification on top of PR 3.

## 8. Open questions

- **T-B3.3** — reload semantics for the allowlist. Decide before
  Phase 4.
- **T-B2.7** — when v2 ships, strict equality + supervised upgrade or
  hello-frame negotiation?
- **B5** needs its own pass when Phase 4 starts.

---

*Maintenance: when you change a trust boundary, update §4 and §5 in
the same PR. When you close a threat, flip its status from
`[planned]` to `[done]` and reference the closing commit.*
