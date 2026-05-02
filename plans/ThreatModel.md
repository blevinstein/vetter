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

**Fix**: move socket under `$XDG_RUNTIME_DIR` / `~/Library/Application
Support/vetter/run/`; client calls `getpeereid()` / `SO_PEERCRED` on
the connected stream and aborts on UID mismatch; `vetterd` holds an
`flock` on `<rundir>/vetterd.pid`, `vet` asserts the locking PID
equals the peer PID.

## T2 — Forged `parsed` bypasses the parser

The daemon evaluates against `req.parsed`, not `req.argv`
(vetterd/src/lib.rs:142). A same-UID process can submit
`argv: ["curl", "https://evil.com"]` with
`parsed.effects: [HttpRequest{ url: "https://example.com" }]`,
get an `Allow`, and have the lie recorded verbatim in the audit log.
Same-UID is exactly the agent context.

**Fix**: drop `parsed` from `VetRequest` (or demote to a hint never
used for policy); daemon re-runs `parsers::dispatch(&req.argv[0])
.parse(&req.argv, …)` and policy runs against the daemon's parsed
output. Stdin gets forwarded as bytes (≤ 1 MiB cap already in place).
Wire break — bumps `PROTOCOL_VERSION` to 2.

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

---

## Sequencing

1. **PR 1 — path + peer-cred + deadlines.** Closes T1 (most of it)
   and T3. `vetter-core/src/paths.rs` (new), `vetterd/src/{paths,
   socket,lib}.rs`, `vet/src/wrap.rs`, `vetter-core/src/wire/mod.rs`
   (`WireError::PeerAuth`).
2. **PR 2 — daemon re-parse.** Closes T2. `vetter-core/src/wire/mod.rs`
   (drop `parsed`, bump `PROTOCOL_VERSION`, forward stdin),
   `vetterd/src/{lib,policy}.rs`.
3. **PR 3 — PID attestation.** Closes the rest of T1.
   `vetterd/src/lib.rs` (flock pid file), `vet/src/wrap.rs` (PID
   attestation against the lock file).

T4 ships separately — small, orthogonal change in
`vetter-core/src/parsers/mod.rs`.
