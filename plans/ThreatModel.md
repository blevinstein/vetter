# Threat Model

Companion to [Overview.md](Overview.md) §6. Overview lists the
high-level posture (protect against an aligned-but-fallible agent;
out of scope: agents that bypass `vet`, sandboxing, TLS inspection).
This file lists the **unmitigated attacks we have decided to fix**,
each with the code site that proves the gap and the change that
closes it. When a threat ships, delete its entry.

---

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

1. **PR — argv0 inode resolution.** Closes T4. Small, orthogonal
  change in `vetter-core/src/parsers/mod.rs`.

Already shipped:

- **T1 path + peer-cred** — socket moved out of `$TMPDIR`, parent dir
forced to `0700`, `vet` runs `getpeereid()`/`SO_PEERCRED` on
connect (PR #5).
- **T1 PID attestation** — `vetterd` holds a POSIX
`fcntl(F_SETLK, F_WRLCK)` on the pidfile for its full lifetime
(`vetter_core::pidfile::PidFileLock` / `acquire`); `vet` opens the
same path and queries the locker pid via `fcntl(F_GETLK)`
(`pidfile::read_locker_pid`), then cross-checks it against the
kernel-reported peer pid (`peer_cred::peer_pid`, `SO_PEERCRED` on
Linux / `LOCAL_PEERPID` on macOS). A same-UID racer that bound
the socket without `fcntl`-locking the pidfile is now caught
before the request frame is sent — `vet` exits 78 with
`WireError::PeerPidMismatch`.
- **T2 daemon re-parse** — wire bumped to v2; `VetRequest` dropped
`parsed`/`command`/`stdin_digest`; `vetterd::handle_connection`
re-runs `parsers::dispatch(&argv[0]).parse(...)` and the matcher
consumes the daemon's `ParsedCommand`. Forging `argv → effects` is
no longer expressible on the wire.
- **T3 read deadlines + inflight cap** — `vetterd::handle_connection`
arms 5 s `set_read_timeout` / `set_write_timeout` per accepted
stream (with `set_nonblocking(false)` so the deadline actually
binds on macOS); `vetterd::accept_loop` keeps an `Arc<AtomicUsize>`
inflight counter under an RAII `InflightGuard`, refusing new
connections beyond `$VETTERD_MAX_INFLIGHT` (default 16) by
accept-then-close so a half-open peer cannot pin every worker.

