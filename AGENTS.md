# AGENTS.md

Guidance for AI coding agents (Cursor, Claude Code, Codex, etc.)
working on the `vetter` project. Humans should read this too.

## Using `vet` while working on this repo

This repo ships a skill that instructs agents to route certain commands
through `vet` itself. Read and follow it before running any commands:

```
~/.claude/skills/use-vet/SKILL.md
```

In short: prefix `curl` calls with `vet` (e.g. `vet curl https://…`).
If `vet` exits 78 (parse error / daemon down), retry without the prefix.
If it exits 77 (denied), stop and tell the user.

The skill lives outside this repo because it applies globally across all
projects, not just `vetter`. The canonical source is kept in the repo at
[`skills/use-vet/SKILL.md`](skills/use-vet/SKILL.md). New users should
install it once for each agent runtime they use:

```bash
# Claude Code
mkdir -p ~/.claude/skills/use-vet
cp skills/use-vet/SKILL.md ~/.claude/skills/use-vet/SKILL.md

# Cursor
mkdir -p ~/.cursor/skills/use-vet
cp skills/use-vet/SKILL.md ~/.cursor/skills/use-vet/SKILL.md
```

## What this project is

`vetter` is a local security gate that sits between an LLM coding agent
and "dangerous" CLI commands. The CLI is named `vet`. See
[plans/Overview.md](plans/Overview.md) for the full architecture, threat
model, and design rationale — read it before making non-trivial changes.

## Where to find things

| You want to... | Read |
|---|---|
| Get oriented quickly (crate map, key file paths) | [plans/RepoMap.md](plans/RepoMap.md) |
| Understand the architecture, types, or rule model | [plans/Overview.md](plans/Overview.md) |
| Find what the next task is | [TODO.md](TODO.md) |
| Find what to test for a given component | [plans/TestingPlan.md](plans/TestingPlan.md) |
| Understand the CLI surface and exit codes | [plans/Overview.md §4](plans/Overview.md) |
| Understand the parser plugin contract | [plans/Overview.md §8](plans/Overview.md) |
| Understand risk-signal heuristics | [plans/Overview.md §9](plans/Overview.md) |
| Understand allowlist YAML schema, matcher, signals | [plans/RepoMap.md §2–5](plans/RepoMap.md) |

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

For a detailed map of every submodule, key file paths, the allowlist
YAML schema, curl parser internals, and signal types, see
[plans/RepoMap.md](plans/RepoMap.md).

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

- **Unit tests live in their own files**, never inline in the source
  module. For each `<crate>/src/path/to/foo.rs` (or `foo/mod.rs`) that
  needs unit tests, put the tests in `<crate>/src/tests/<flattened>.rs`
  (e.g. `src/matcher/loader.rs` → `src/tests/matcher_loader.rs`,
  `src/parsers/curl/state.rs` → `src/tests/parsers_curl_state.rs`),
  and declare in the source file:
  ```rust
  #[cfg(test)]
  #[path = "tests/foo.rs"]            // or "../tests/...", "../../tests/..."
  mod tests;
  ```
  Inside the tests file, `super::*` resolves to the source module's
  namespace, so private items remain reachable. **Do not** reintroduce
  inline `#[cfg(test)] mod tests { ... }` blocks — having a single
  discoverable `tests/` directory per crate keeps grep-for-test-name
  cheap and the source files focused on production code.
  Cross-crate or black-box tests still belong under `<crate>/tests/`
  as Cargo integration tests.
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
