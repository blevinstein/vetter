# Phase 4 Notes

Operational notes for the Phase 4 macOS approver UI. Lives next to
`Overview.md` so future agents working on Phase 4 follow-ups can find
the smoke-test procedure and the manual-only coverage gaps without
having to re-derive them.

## What PR 1 covers

- All-in-Rust menu-bar `.app` bundle (`Vetter.app`).
- `UNUserNotificationCenter` notifications with **Approve** /
  **Reject** action buttons; clicking either resolves the matching
  pending request.
- Pending-queue spine + `Notifier` trait + `MockNotifier` for
  test-driven coverage.
- Refactored `policy::evaluate` returning `PolicyOutcome::{Auto,
  Prompt}` (the Phase 3a stub-deny is gone).
- Audit log records the resolved decision, not the stub.

## What's deferred to follow-up Phase 4 PRs

- Popover with the full pending-request list (§8.5 detail view).
- Notification coalescing (banner-then-menu-bar after the first
  prompt).
- Real Developer-ID signing + notarisation + Homebrew tap.
- `Allowlist…` action (Phase 5 territory).
- `--dry-run` UX polish (subtitle currently says "dry run", but the
  popover should also visually distinguish them).
- Sandbox entitlements file.

## Building the .app

```sh
tools/build-app.sh           # debug
tools/build-app.sh --release # release
# outputs target/Vetter.app
```

The script:

1. Runs `cargo build -p vetterd` for the chosen profile.
2. Lays out `Vetter.app/Contents/{MacOS,Resources}` and copies the
   binary into `Contents/MacOS/vetterd`.
3. Renders `vetterd/resources/Info.plist.template` (substitutes
   `__VERSION__` from Cargo.toml).
4. Ad-hoc signs the bundle with `codesign --sign -`. This is enough
   for `UNUserNotificationCenter` to recognise the bundle on the
   developer's own machine; a Developer-ID identity is required to
   distribute it.

The Info.plist sets:

- `LSUIElement=true` → menu-bar / accessory app, no dock icon.
- `CFBundleIdentifier=dev.vetter.daemon` → stable id for
  `UNUserNotificationCenter` permission grants.
- `LSEnvironment.VETTERD_NOTIFIER=mac` → forces the AppKit notifier
  irrespective of the launching shell's environment.

## Manual smoke test

This is the only coverage of the real `UNUserNotificationCenter`
integration; CI uses the [`MockNotifier`](../vetterd/src/notifier/mock.rs)
driven by [`vetterd/tests/daemon_e2e_prompt.rs`].

1. `tools/build-app.sh --release`
2. `open target/Vetter.app`
   - On first launch the macOS notification permission dialog
     appears; choose **Allow**.
   - The menu-bar status item labelled **Vetter** appears in the
     top-right.
3. `cargo build -p vet --release && export PATH="$PWD/target/release:$PATH"`
4. From any shell, run a command that hits prompt-class:
   `vet curl https://prompt-test.example/` — assuming no allow rule
   matches, a notification banner pops with **Approve** / **Reject**.
5. Click **Approve**: the curl request executes (`vet` exec's curl
   on allow). Click **Reject**: `vet` exits 77 with the deny reason.
6. Verify the audit log:
   `tail -n2 ~/Library/Logs/vetter/audit.log` — both decisions are
   recorded with the user-driven reason.
7. Click the menu-bar **Vetter → Quit Vetter** to shut the daemon
   down cleanly. (`launchctl bootout` is the right tool when the
   daemon is being run as a LaunchAgent; that wiring lands in a
   follow-up PR.)

## Troubleshooting

- **No notification appears.** Check System Settings → Notifications
  → Vetter is enabled (Allow Notifications on, Banners or Alerts
  selected). Re-`open Vetter.app` to re-trigger the
  `requestAuthorizationWithOptions` call, which surfaces the
  permission dialog if it was previously denied.
- **Status item missing from menu bar.** macOS hides extra status
  items behind the system clock when the bar is full; click the
  clock or use [Bartender] to verify Vetter is registered.
- **`vet` hangs forever.** The notifier delegate isn't resolving the
  pending entry; the audit log will eventually receive a
  `daemon shutting down` reason when the daemon is killed. Capture
  Console.app logs filtered on `vetterd` for the
  `addNotificationRequest failed` message.

[Bartender]: https://www.macbartender.com/
