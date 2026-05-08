<!--
Opt-in PR template for MSRV bumps. Append `?template=msrv.md` to the
GitHub PR-create URL to use it. Toolchain changes must be explicit per
TODO.md §H3 — Supply chain.
-->

## MSRV bump

Bumping the workspace minimum supported Rust version from `<old>` to `<new>`.

### Why

<!--
One short paragraph describing what forces the bump. Examples:
  - "stable Rust shipped `&str::trim_ascii` (1.80) and we want to use it in
    `vetter-core::render::sanitize_for_display`"
  - "edition 2024 stabilises in 1.85; the workspace upgrades to use `let`
    chains throughout matcher code"
  - "transitive dep `foo 0.42` requires 1.96; pinning the older 0.41 keeps
    blocking advisory-FIX-2026-1234"

If the bump is dep-driven, link the upstream release notes / changelog.
-->

### Checklist

- [ ] `Cargo.toml` `workspace.package.rust-version` updated to the new MSRV.
- [ ] `.github/workflows/ci.yml` `msrv` job's `dtolnay/rust-toolchain@<ver>`
      pinned to the new MSRV.
- [ ] `Cargo.lock` regenerated against the new toolchain (`cargo generate-lockfile`)
      if any dep version changed.
- [ ] No new compiler warnings on the new MSRV toolchain
      (`cargo +<ver> build --workspace --all-targets --all-features`).
- [ ] User-facing reason captured above; not "because we felt like it".

### Test plan

- [ ] CI `msrv` job is green on this PR.
- [ ] CI `test` matrix (stable, ubuntu + macOS) is green on this PR.
- [ ] Local: `rustup toolchain install <ver> && cargo +<ver> test --workspace --all-features`.
