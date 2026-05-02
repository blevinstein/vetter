# Threat Model

Companion to [Overview.md](Overview.md) §6. Overview lists the
high-level posture (protect against an aligned-but-fallible agent;
out of scope: agents that bypass `vet`, sandboxing, TLS inspection).
This file lists the **unmitigated attacks we have decided to fix**,
each with the code site that proves the gap and the change that
closes it. When a threat ships, delete its entry.

---

## T1 — Daemon impersonation

A same-UID process binds `$TMPDIR/vetter.sock` before `vetterd`
starts. `vet` connects, dumps the full `VetRequest` (argv, parsed
effects, cwd, stdin digest) to the attacker, and receives a forged
`Allow`. `vet` then `execvp`s the unverified command. Allowlist,
audit log, and UI are all bypassed — and the threat model in
Overview.md §6 does *not* cover this case (it covers agents that
bypass `vet`, not agents that impersonate `vetterd`).

`vet` performs no peer-cred check on `connect()` (vet/src/wrap.rs:154)
and the socket lives in a path the attacker can race for.

**Fix**: move socket under `$XDG_RUNTIME_DIR` / `~/Library/Application Support/vetter/run/`; client calls `getpeereid()` / `SO_PEERCRED` on
the connected stream and aborts on UID mismatch; `vetterd` holds an
`flock` on `<rundir>/vetterd.pid`, `vet` asserts the locking PID
equals the peer PID.

## T3 — Resource exhaustion

`vetterd` spawns one OS thread per accepted connection
(vetterd/src/lib.rs:119) with no read deadline on the request frame.
A trickle of half-open connections holds workers indefinitely. Daemon
becomes unresponsive → `vet` fails closed → user can't run anything.

**Fix**: 5 s read deadline on the request frame; cap inflight workers
(default 16, configurable via `VETTERD_MAX_INFLIGHT`); accept-and-
immediately-close above the cap.

## T4 — argv0 spoofing

`ln /bin/bash /tmp/curl && vet /tmp/curl …` — basename dispatch
routes to the curl parser, but exec runs bash. Parser-emitted effects
describe a curl call that won't happen.

**Fix**: dispatcher resolves argv[0] to a real path and refuses
unknown inodes (or matches against an expected install path list).

## T5 — Lookalike / homograph domain

An adversarial prompt instructs the agent to `curl https://g0ogle.com/…`
(or similar typosquat / homograph). The call may match a permissive
allowlist rule (e.g. `host: "*.com"`) and auto-allow without the human
ever noticing the substituted character.

**Partial mitigation shipped**: the `UnknownHost` risk signal
(`SignalKind::UnknownHost`, `plans/Overview.md` §9) fires whenever an
`HttpRequest` targets a host absent from the layered known-hosts list
(`~/.vet/known-hosts.yaml`, `<repo>/.vet/known-hosts.yaml`, built-in
list). This surfaces a yellow warning badge in the render block and forces
human attention on the pending-request popover, even when a permissive
allow rule would otherwise auto-approve.

Limitations: the signal is suppressed if the lookalike host happens to
match a known-hosts glob (unlikely but possible), and it cannot catch
attacks where the agent constructs the URL dynamically at runtime from
substrings (the parser only sees the final URL). A full mitigation would
require homograph-specific detection (Levenshtein distance to known hosts,
Unicode confusable analysis) — that is future work.

---

## Sequencing

Remaining work, smallest blast radius first:

1. **PR — read deadlines + inflight cap.** Closes T3.
  `vetterd/src/{lib,socket}.rs`.
2. **PR — PID attestation.** Closes the rest of T1.
  `vetterd/src/lib.rs` (flock pid file), `vet/src/wrap.rs` (PID
   attestation against the lock file).
3. **PR — argv0 inode resolution.** Closes T4. Small, orthogonal
  change in `vetter-core/src/parsers/mod.rs`.

Already shipped:

- **T1 path + peer-cred** — socket moved out of `$TMPDIR`, parent dir
forced to `0700`, `vet` runs `getpeereid()`/`SO_PEERCRED` on
connect (PR #5).
- **T2 daemon re-parse** — wire bumped to v2; `VetRequest` dropped
`parsed`/`command`/`stdin_digest`; `vetterd::handle_connection`
re-runs `parsers::dispatch(&argv[0]).parse(...)` and the matcher
consumes the daemon's `ParsedCommand`. Forging `argv → effects` is
no longer expressible on the wire.

