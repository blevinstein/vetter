# macOS app — build, smoke test, troubleshooting, AppKit lessons

Operational guide for the bundled macOS daemon (`Vetter.app`). Covers
how the bundle is laid out and built, the manual smoke procedure that
exercises the real `UNUserNotificationCenter` integration the
`MockNotifier` can't reach, the troubleshooting checklist when the
menu-bar UI misbehaves, and the AppKit pitfalls we have already
hit and don't want to re-discover.

For the *visual* design of the popover (card layout, signal pills,
host-trust palette, dry-run wrapper, button HIG choices) see
[ApprovalUI.md](ApprovalUI.md). This doc is operational; that doc is
the spec.

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
   put one directory on PATH). The layout step lives in
   `tools/_bundle_layout.sh` and is shared with the release pipeline
   so dev and distribution bundles can never drift.
3. Renders `vetterd/resources/Info.plist.template` (substitutes
   `__VERSION__` from Cargo.toml).
4. Ad-hoc signs the bundle with `codesign --sign -` (`--deep` so the
   `vet` helper is signed too). This is enough for
   `UNUserNotificationCenter` to recognise the bundle on the
   developer's own machine; a Developer-ID identity is required to
   distribute it.

For a Developer-ID signed + notarised + stapled bundle (the artifact
the Homebrew cask serves), use `tools/release.sh` and follow
[Release.md](Release.md). That doc owns the Apple-side prerequisites
(Developer Program membership, certificate, Notary API key, Team ID),
the cross-arch toolchain setup, and the cask-publish flow.

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
7. **Concurrent prompts (coalescing).** Run two `vet curl` commands
   at once. Only the **first** request raises a banner — the second
   coalesces into the menu-bar (spec §7: "we don't spam banners").
   The menu-bar badge reads ` 2` and the popover lists both cards.
   After resolving the first via its banner the queue still has one
   pending request; the badge drops to ` 1` but no fresh banner
   fires (resolving doesn't re-trigger coalescing). Open the popover
   to clear the second. If a third `vet curl` is issued after the
   queue is fully empty, that **does** raise a fresh banner (the
   "next burst is `was_empty_before == true` again" property pinned
   by `submit_marks_first_entry_as_empty_before`).
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
- **`vet daemon start` succeeds but no menu-bar icon appears.** As
  of the "default to mac" flip the daemon refuses to come up
  outside an `.app` bundle: you should now see
  `vetterd: VETTERD_NOTIFIER=mac requires running inside a
  code-signed .app bundle (...)` and a `vet daemon start` failure
  ("vetterd exited before binding socket"). Either launch via
  `open target/Vetter.app` (the supported path) or, for lifecycle
  experiments only, prepend `VETTERD_NOTIFIER=noop vet daemon
  start` to opt out of the UI entirely.
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

## AppKit pitfalls we've hit

Captured here so future macOS UI work doesn't re-discover them. All
three landed as runtime crashes that the `MockNotifier`-driven
integration suite couldn't catch, because that path skips the real
AppKit run loop entirely.

- **Don't `removeArrangedSubview:` after `removeFromSuperview`.**
  `[NSView removeFromSuperview]` already removes the view from both
  `subviews` *and* `arrangedSubviews`. Calling
  `[NSStackView removeArrangedSubview:]` afterwards trips an
  `NSAssertionHandler` failure inside
  `_removeView:animated:removeFromViewHierarchy:` and aborts the
  process. The canonical "clear all arranged subviews" idiom is
  `for v in cards.arrangedSubviews() { v.removeFromSuperview() }`
  — let `NSStackView` do its own bookkeeping.
- **Don't shut down via `[NSApp terminate:]`.** `terminate:` calls
  `exit()` after its delegate ceremony and never returns control to
  `[NSApp run]`, which means the cleanup tail in
  [`vetterd::run`](../vetterd/src/lib.rs) (notifier shutdown, socket
  removal, pidfile removal) is skipped — the daemon dies but leaks
  its runtime files, and `vet daemon stop`'s 2 s timeout is
  routinely overshot by `terminate:`'s ceremony. Funnel every
  shutdown path (SIGTERM, SIGINT, popover Quit) through the shared
  shutdown atomic; the observer thread in
  [`runloop::run_app_kit`](../vetterd/src/runloop/mod.rs) translates
  it into `[NSApp stop:]` plus a no-op `applicationDefined` event so
  `[NSApp run]` returns naturally and the cleanup runs.
- **`NSStackView` as `NSScrollView.documentView` needs explicit
  constraints.** `NSStackView` is Auto Layout only
  (`translatesAutoresizingMaskIntoConstraints == false`), so set as
  the document view of an `NSScrollView` with no anchor constraints
  it stays at zero size and the entire subtree renders invisible —
  the popover looks empty even with pending requests in the queue.
  Pin `leadingAnchor` / `trailingAnchor` / `topAnchor` to
  `scroll.contentView()` and `widthAnchor` to `scroll.widthAnchor()`
  (so vertical scroll only). Same gotcha applies the other
  direction: any frame-sized child (e.g. the per-card
  `NSTextView::scrollableTextView`) added to an autolayout
  `NSStackView` loses its frame and collapses to its intrinsic
  height — pin its `heightAnchor` explicitly. See
  [`runloop::popover::Popover::new`](../vetterd/src/runloop/popover.rs)
  for the canonical fix.

The general rule both lessons argue for: **the AppKit code path is
the part of the daemon least covered by integration tests.** Any
new dynamic UI (popover variants, allowlist picker, etc.) should
be exercised manually via the smoke test above before merge, *and*
extracted into pure functions wherever feasible so the test suite
can cover the not-AppKit half (e.g. queue → card-data lowering, but
not the `NSView` assembly).
