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
7. **Banner Allowlist… path.** Run another prompt-class command.
   While the banner is up, expand its actions (long-press / right-
   click on macOS Big Sur+, or hover-then-Options on later macOS),
   and click **Allowlist…**. The Vetter app must come forward
   (LSUIElement apps don't auto-foreground from a notification
   action), the picker `NSAlert` should appear listing Exact /
   PathGlob / MethodHost tiers, pick one, click **Add to user
   allowlist**. The follow-up alert should report
   `Auto-approved 1 pending request(s)` and the original banner
   should be cleared from Notification Center automatically (the
   `remove_delivered_for_ids` call in `persist_rule_async`). The
   blocked `vet` should now exec curl. `~/.config/vet/allowlist.yaml`
   has the new rule appended.
8. **Banner Trust host… path.** Run `vet curl https://fresh-host.example/`
   against a host that is NOT in your known-hosts. Expand the
   banner actions; **Trust host…** should be present (the with-
   unknown-host category — for already-trusted hosts the action
   is *absent*). Click it, pick a tier, **Trust this host**.
   `~/.config/vet/known_hosts.yaml` gets the entry; the request
   stays pending (trusting a host doesn't auto-approve), so click
   **Approve** on the still-visible banner to release `vet`.
9. **Concurrent prompts (coalescing).** Run two `vet curl` commands
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
10. **Audit log.** `tail -n6 ~/Library/Logs/vetter/audit.log` should
    show a mix of `approved via notification`, `approved via popover`,
    `rejected via notification`, `rejected via popover`, and
    `auto-approved by newly added rule` reasons alongside the
    existing `matched rule …` lines.
11. **Quit.** Click the popover's **Quit Vetter** button. The daemon
    shuts down cleanly (socket and pidfile removed).
12. **Autostart on login.** Open the popover and tick **Start at
    login**. `vet doctor` should now show
    `OK   autostart  enabled (login item registered)` and System
    Settings → General → Login Items should list **Vetter**. Log
    out + log back in (or restart): the menu-bar shield should
    reappear without you opening anything. Untick the checkbox
    and reboot to confirm the unregistration also takes effect
    (or run `vet daemon autostart disable` from the CLI).
13. **Inspect file input from popover.**
    `printf '{"x":1}\n' > /tmp/sample.json && vet curl -d @/tmp/sample.json https://prompt-test.example/`.
    Open the popover; the body row (`from file`) and the `read`
    row both show the path with a small **↗** glyph button to its
    right. Click either button — the JSON should open in your
    default app for `.json` (TextEdit / VS Code / Quick Look).
    Sanity-check: `Body::FromStdin` cards (`vet curl -d @- …` with
    a piped payload) and `FileWrite` rows (`vet curl -o /tmp/out
    https://prompt-test.example/`) intentionally do **not** show
    the button this round — write paths may not exist yet, and
    "Reveal in Finder" is a follow-up. Approve the request to
    finish.

## Autostart on login

The popover footer carries a **Start at login** checkbox alongside
the **Quit Vetter** button. Ticking it does three things, in order:

1. Persists the user preference to `~/.vet/settings.yaml`
   (`autostart: true`). The file lives next to `allowlist.yaml` and
   `known-hosts.yaml` and is mode `0600` from the moment it is
   first written.
2. Calls `[SMAppService.mainApp registerAndReturnError:]` so launchd
   knows to relaunch `Vetter.app` at every login. The same call is
   what populates the **Login Items** entry under System Settings
   → General → Login Items.
3. Re-reads `[SMAppService.mainApp status]` and snaps the checkbox
   back if Apple returned an error (typically because the user has
   not yet *approved* Vetter in System Settings → Login Items).
   The visible state never claims a setting the OS rejected.

Unticking inverts the same flow with `unregisterAndReturnError:`.

The CLI mirror is `vet daemon autostart enable | disable | status`,
which routes through the daemon's admin socket so headless setups
(provisioning scripts, dotfiles installers) can opt in without
opening the popover.

On daemon startup `vetterd::run` calls
[`autostart::reconcile_with_settings`](../vetterd/src/autostart.rs)
to converge the OS-level state with the persisted preference. This
is the recovery path for two cases:

- The user disabled autostart from **System Settings → Login Items**
  while the daemon was offline. The next launch obeys their
  decision (we don't silently re-register on top of a user opt-out).
- The user hand-edited `~/.vet/settings.yaml` to flip
  `autostart: true`. The next launch applies the registration
  without forcing them to open the popover.

### Debugging

- `launchctl print gui/$(id -u) | grep dev.vetter.daemon` shows the
  current launchd registration for the user session. An empty
  result means the bundle is not currently registered as a Login
  Item.
- `vet doctor` includes an `autostart` row that consults the live
  `[SMAppService.mainApp status]` via the admin socket — useful
  when reconciling user reports of "I ticked the box but nothing
  happens at next login".
- Apple keeps the user-approval state for `SMAppService` in System
  Settings → General → Login Items. If a user previously denied
  the registration there, our `register` call surfaces as
  `RequiresApproval` and `vet doctor` flags it as a `WARN`.

`SMAppService` is macOS 13+; `Info.plist.template` declares
`LSMinimumSystemVersion = 13.0` so older macOS hosts don't even
launch the bundle and hit the missing-class panic from
`class!(SMAppService)` at runtime.

## Troubleshooting

- **No notification appears.** Check System Settings → Notifications
  → Vetter is enabled (Allow Notifications on, Banners or Alerts
  selected). Re-`open Vetter.app` to re-trigger the
  `requestAuthorizationWithOptions` call, which surfaces the
  permission dialog if it was previously denied. If Vetter entries
  are landing in Notification Center but never appearing as banners,
  the usual culprits are macOS delivery policy rather than a daemon
  bug:
  - verify **Alert Style** is set to **Banners** or **Alerts**
    (not **None**)
  - disable **Scheduled Summary**, or at least remove Vetter from it
  - disable **Focus** entirely while testing, or explicitly allow
    **Vetter** inside the active Focus mode; a Focus rule can suppress
    banners while still allowing the notification to be delivered
    quietly into Notification Center
  - check Notification Center manually (clock / date in the menu bar)
    to distinguish "banner suppressed" from "notification never posted"
  - if you are screen-sharing, presenting, or mirroring displays,
    temporarily stop that session; macOS often suppresses banners in
    those modes even when the app is authorised
  When debugging, `log show --last 2m --predicate 'process ==
  "vetterd"' --style compact` is the fastest truth source: a healthy
  delivery attempt looks like `Adding notification request ...`
  followed by `Added notification request: [ hasError: 0 ... ]`.
- **Status item missing from menu bar.** macOS hides extra status
  items behind the system clock or notch when the bar is full;
  click the clock or use [Bartender] to verify Vetter is registered.
  The icon is template-tinted (light/dark adaptive) — it shouldn't
  be invisible against either menu-bar appearance. In practice the
  most common failure is simple crowding: quit or hide a few status
  items, then relaunch Vetter and confirm the shield icon appears.
  If a third-party menu-bar manager (Bartender, Hidden Bar, Ice, ...)
  is installed, make sure Vetter is not explicitly hidden there.
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
  confirm the queue plumbing is healthy. Also remember the notifier
  intentionally coalesces bursts: only the first request submitted to
  an empty queue raises a banner; later requests while one is already
  pending only update the menu-bar badge + popover. If "nothing
  appears" after a previous prompt got stranded, inspect the live
  queue with `vet daemon list`, clear the stale entry via the popover,
  then retry with one fresh request on an empty queue.
- **`vet` hangs forever.** The notifier delegate isn't resolving the
  pending entry; the audit log will eventually receive a
  `daemon shutting down` reason when the daemon is killed. Capture
  Console.app logs filtered on `vetterd` for the
  `addNotificationRequest failed` message. If the logs instead show
  `Added notification request: [ hasError: 0 ... ]` while the user
  saw no banner, the problem is almost certainly macOS notification
  policy (Focus, Scheduled Summary, alert style, presentation mode)
  rather than Vetter's posting path.

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
