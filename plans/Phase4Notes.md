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

## What PR 2 adds

- Menu-bar status-item icon (`checkmark.shield` SF Symbol, template
  tinted) with a pending-count badge — when N requests are
  outstanding the icon switches to the filled variant and the badge
  reads ` (N)`.
- An `NSPopover` anchored to the status-item icon, listing every
  pending request as a card with the §8.5 detail rendering plus
  per-card **Approve** / **Reject** buttons. Cards close their
  banner via `removeDeliveredNotificationsWithIdentifiers:` when the
  user clicks either button so a resolved request never lingers in
  Notification Center.
- Click-through routing: tapping the body of a notification banner
  (the "default action") now opens the popover and scrolls the
  matching card into view; the request stays **pending** until the
  user clicks Approve / Reject in the popover.
- Quit lives in the popover footer instead of the previous status-bar
  context menu (the button's action slot is now needed for
  `togglePopover:`).
- `PendingQueue` carries the §8.5 rendered detail per entry plus a
  change listener so the popover and badge auto-refresh on
  submit / resolve / cancel.

## What's deferred to follow-up Phase 4 PRs

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

1. Runs `cargo build -p vetterd -p vet` for the chosen profile.
2. Lays out `Vetter.app/Contents/{MacOS,Resources}` and copies both
   binaries into `Contents/MacOS/` (`vetterd` is the bundle's main
   executable; `vet` rides along as a CLI helper so a developer can
   put one directory on PATH).
3. Renders `vetterd/resources/Info.plist.template` (substitutes
   `__VERSION__` from Cargo.toml).
4. Ad-hoc signs the bundle with `codesign --sign -` (`--deep` so the
   `vet` helper is signed too). This is enough for
   `UNUserNotificationCenter` to recognise the bundle on the
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

1. `tools/build-app.sh --release` (builds `vetterd` + `vet` and
   packages both into the bundle).
2. `open target/Vetter.app`
   - On first launch the macOS notification permission dialog
     appears; choose **Allow**.
   - The menu-bar status item with the **shield** icon appears in
     the top-right (no dock icon, `LSUIElement=true`).
3. `export PATH="$PWD/target/Vetter.app/Contents/MacOS:$PATH"` to
   put the bundled `vet` on PATH.
4. **Notification-button path.** Run a command that hits prompt-class:
   `vet curl https://prompt-test.example/` — assuming no allow rule
   matches, a notification banner pops with **Approve** / **Reject**.
   Clicking either resolves the request and the banner is removed
   from Notification Center.
5. **Popover path.** Run another prompt-class command. While the
   banner is up, click the menu-bar shield icon. The popover should
   appear with one card listing the §8.5 detail and **Approve** /
   **Reject** buttons. Clicking either resolves the request, the
   banner is dismissed, and the badge count drops.
6. **Click-through path.** Run a third prompt-class command. Click
   the *body* of the notification (not Approve / Reject). The
   popover should open with the matching card in view; `vet` is
   still blocked. Click Approve in the popover; `vet` exec's curl.
7. **Concurrent prompts.** Run two `vet curl` commands at once. The
   menu-bar badge reads ` 2`; the popover lists both cards; the
   banner for each appears in turn. Resolving each card decrements
   the badge.
8. **Audit log.** `tail -n6 ~/Library/Logs/vetter/audit.log` should
   show a mix of `approved via notification`, `approved via popover`,
   `rejected via notification`, and `rejected via popover` reasons
   alongside the existing `matched rule …` lines.
9. **Quit.** Click the popover's **Quit Vetter** button. The daemon
   shuts down cleanly (socket and pidfile removed).

## Troubleshooting

- **No notification appears.** Check System Settings → Notifications
  → Vetter is enabled (Allow Notifications on, Banners or Alerts
  selected). Re-`open Vetter.app` to re-trigger the
  `requestAuthorizationWithOptions` call, which surfaces the
  permission dialog if it was previously denied.
- **Status item missing from menu bar.** macOS hides extra status
  items behind the system clock or notch when the bar is full;
  click the clock or use [Bartender] to verify Vetter is registered.
  The icon is template-tinted (light/dark adaptive) — it shouldn't
  be invisible against either menu-bar appearance.
- **Popover never opens.** Make sure the bundle was launched via
  `open target/Vetter.app`; running `target/Vetter.app/Contents/MacOS/vetterd`
  directly skips Launch Services, so AppKit's NSStatusBar is up but
  may be in a degraded state without an event-loop foreground app
  registration.
- **Card lists "stuck" pending requests.** A request the daemon
  cannot deliver to the UI (e.g. notification authorization denied
  AND the popover wasn't open) sits in the pending queue until the
  user opens the popover and clicks Approve / Reject. Running
  `vet curl …` while the popover is open is the easiest way to
  confirm the queue plumbing is healthy.
- **`vet` hangs forever.** The notifier delegate isn't resolving the
  pending entry; the audit log will eventually receive a
  `daemon shutting down` reason when the daemon is killed. Capture
  Console.app logs filtered on `vetterd` for the
  `addNotificationRequest failed` message.

[Bartender]: https://www.macbartender.com/
