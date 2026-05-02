# AGENTS.md

Guidance for AI coding agents (Cursor, Claude Code, Codex, etc.)
working on the `vetter` project. Humans should read this too.

## What this project is

`vetter` is a local security gate that sits between an LLM coding agent
and "dangerous" CLI commands. The CLI is named `vet`. See
[plans/Overview.md](plans/Overview.md) for the full architecture, threat
model, and design rationale — read it before making non-trivial changes.

## Where to find things

| You want to... | Read |
|---|---|
| Understand the architecture, types, or rule model | [plans/Overview.md](plans/Overview.md) |
| Find what the next task is | [TODO.md](TODO.md) |
| Find what to test for a given component | [plans/TestingPlan.md](plans/TestingPlan.md) |
| Understand the CLI surface and exit codes | [plans/Overview.md §4](plans/Overview.md) |
| Understand the parser plugin contract | [plans/Overview.md §8](plans/Overview.md) |
| Understand risk-signal heuristics | [plans/Overview.md §9](plans/Overview.md) |

If you're picking up a fresh task: open [TODO.md](TODO.md) first, find
the in-progress (`[~]`) phase or the next not-started (`[ ]`) phase,
and consult the cited section of `Overview.md` / `TestingPlan.md` for
detail.

## Workspace layout

Rust workspace, `cargo` for everything:

- `vetter-core/` — shared library: `ParsedCommand`, `Effect`, parser
  registry, renderer, risk analyzer, wire types. The single API every
  downstream consumer reads from.
- `vet/` — primary CLI binary; agent harnesses allowlist `vet *`.
- `vetterd/` — long-running per-user daemon (Phase 3+).

## How to build and test

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets               -- -D warnings
cargo test  --workspace --all-features
```

The `--all-features` runs enable the test-only `noop` parser used by
integration tests in `vetter-core/tests/`. CI runs both feature
configurations.

## Conventions

- Snapshot tests use `insta`. After intentional output changes, run
  `INSTA_UPDATE=always cargo test --workspace --all-features` and
  commit the regenerated `.snap` files alongside the code change.
  Never let CI silently update snapshots.
- The renderer is command-agnostic: it iterates `ParsedCommand.effects`
  and never branches on `command`. New parsers add a file under
  `vetter-core/src/parsers/<name>.rs` and a fixture corpus under
  `vetter-core/tests/corpus/<name>/`. No core changes.
- Parser-specific risk signals (e.g. curl `--insecure`) are pushed by
  the parser into `ParsedCommand.signals`. The generic analyzer in
  `vetter-core::signals::analyze` only handles items from
  [plans/Overview.md §9](plans/Overview.md) generic list.
- Failing closed beats failing open. If a parser cannot represent an
  invocation precisely, return `ParseError` so `vet` refuses to run
  rather than silently mis-vetting.
- Exit codes per [plans/Overview.md §4](plans/Overview.md): `0` allowed
  + ran, `77` denied, `78` config error, otherwise the wrapped
  command's exit code.

## When you finish a task

1. Update [TODO.md](TODO.md): change `[ ]` to `[x]` for the items you
   completed; flip the phase tag to `[x] done` if everything in that
   phase is now ticked.
2. Run the full verification block above; do not skip clippy or fmt.
3. If you added new tests or changed snapshots, confirm the diffs are
   intentional before committing.

## Things to avoid

- Branching on `ParsedCommand.command` outside of parser code (renderer,
  analyzer, matcher must stay command-agnostic).
- Leaking secret header values into rendered output, audit logs, or
  test fixtures. The redaction list in
  [vetter-core/src/render/redact.rs](vetter-core/src/render/redact.rs)
  is the source of truth.
- Adding a TTY prompt path to `vet`. Approvals must go through the
  daemon's separate-channel UI; see [plans/Overview.md §2, §4](plans/Overview.md).
- Editing `plans/Overview.md` or `plans/TestingPlan.md` to fit the
  implementation. They are the spec; raise discrepancies in PR
  description or open questions section, don't silently rewrite the
  spec.
