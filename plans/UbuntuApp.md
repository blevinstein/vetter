# Ubuntu app — build, smoke test, troubleshooting, GTK lessons

Operational guide for the Linux daemon, mirroring
[MacOSApp.md](MacOSApp.md). Covers how the `.deb` is laid out and
built, the manual smoke procedure that exercises the real D-Bus +
StatusNotifierItem + GTK plumbing the mock notifier can't reach, the
troubleshooting checklist when the tray / popover misbehaves, and
the GTK / gobject pitfalls we want to write down once rather than
re-discover each time.

For the *visual* design of the popover (card layout, signal pills,
host-trust palette, dry-run wrapper, button HIG choices) see
[ApprovalUI.md](ApprovalUI.md). The card catalogue is shared between
macOS and Linux; the GTK port restates the contract in
`pango::AttrList` / `gtk::CssProvider` terms but the data layer is
unchanged.

## Building

The two supported flows on Ubuntu are *develop from source* and
*install from the PPA*. Distribution-quality builds (the `.deb`
uploaded to Launchpad) live in [Release.md](Release.md); use this
doc for local hacking.

### Build dependencies

```sh
sudo apt-get install -y \
    libgtk-4-dev libglib2.0-dev libdbus-1-dev pkg-config build-essential
```

`gtk4-rs` and `zbus` need the system development headers above; the
rest of the workspace is pure Rust. CI installs the same set in the
Ubuntu test job.

### Cargo build

```sh
cargo build --release -p vetterd -p vet
```

Two binaries land in `target/release/`. Unlike macOS there is no
bundle layout step — the `.deb` (built below) packs them straight
into `/usr/bin/`.

### `.deb` build

```sh
cargo install cargo-deb           # one-time
cargo deb -p vetterd              # outputs target/debian/vetter_<VERSION>_amd64.deb
```

`cargo-deb` reads the `[package.metadata.deb]` block in
[`vetterd/Cargo.toml`](../vetterd/Cargo.toml) and ships:

- `/usr/bin/vet`, `/usr/bin/vetterd`
- `/usr/lib/systemd/user/vetter.service` — systemd user unit;
  `WantedBy=default.target`, `Environment=VETTERD_NOTIFIER=linux`
- `/etc/xdg/autostart/vetter.desktop` — autostart entry so the tray
  comes up on login for users running a graphical session
- `/usr/share/icons/hicolor/scalable/apps/dev.vetter.daemon.svg` —
  the shield tray icon, theme-tinted via `gtk::CssProvider`
- A `postinst` that runs `systemctl --user --global enable
  vetter.service` so newly created accounts inherit the
  auto-start behaviour without re-running the package

Verifying the build before upload:

```sh
sudo dpkg -i target/debian/vetter_*.deb
systemctl --user start vetter.service
journalctl --user -u vetter.service -n 50
```

## Manual smoke test

This is the only coverage of the real D-Bus + tray + GTK
integration; CI uses the [`MockNotifier`](../vetterd/src/notifier/mock.rs)
driven by [`vetterd/tests/daemon_e2e_prompt.rs`].

1. Build the `.deb` per the section above and `sudo dpkg -i …`.
   (Alternatively: `cargo install --path vetterd && cargo install
   --path vet`, then `systemctl --user --user-unit
   vetterd@.service start` against a hand-rolled unit; the `.deb`
   path is what real users see.)
2. Log out and back into the GNOME session so `systemd --user`
   re-reads its unit catalogue and starts `vetter.service`.
   Confirm with `systemctl --user status vetter.service` — the
   line should read `Active: active (running)`.
3. **Tray icon.** A shield icon appears in the GNOME top bar (with
   `gnome-shell-extension-appindicator` installed) or the KDE/Plasma
   system tray. Right-click → **Open vetter window…**, **Pending: 0**,
   **Quit Vetter**. A click on the icon body opens the popover
   window; closing it returns the tray to the idle state.
4. **Notification-button path.** Run `vet curl https://prompt-test.example/`.
   A desktop notification appears with **Approve** and **Reject**
   buttons. Clicking either resolves the request, the tray badge
   clears, and `vet` returns 0 (Approve, then `curl`'s own exit
   code) or 77 (Reject).
5. **Popover path.** Run another prompt-class command. While the
   notification is up, click the tray icon. The popover window
   opens with one card listing the §8.5 detail and **Approve** /
   **Reject** buttons. Clicking either resolves the request, the
   tray badge drops, and the notification disappears from the
   shell history.
6. **Click-through path.** Run a third prompt-class command. Click
   the *body* of the notification (not Approve / Reject). The
   popover should open with the matching card scrolled into view;
   `vet` is still blocked. Click Approve in the popover; `vet`
   exec's curl.
7. **Concurrent prompts (coalescing).** Run two `vet curl` commands
   at once. Only the **first** request raises a banner — the
   second coalesces into the tray (spec §7: "we don't spam
   banners"). The tray badge reads `2` and the popover lists both
   cards.
8. **Headless fallback.** Open an SSH session into the same machine
   (so `$DISPLAY` and `$DBUS_SESSION_BUS_ADDRESS` are unset). Run
   `vet curl https://prompt-test.example/`. The request blocks;
   `vet daemon list` from the SSH terminal shows it; running
   `vet daemon approve <id>` resolves it without ever touching the
   GUI. `vet daemon reject <id>` is the parallel path.
9. **Audit log.** `tail -n6 ~/.local/state/vetter/audit.log` should
   show a mix of `approved via notification`, `approved via
   popover`, `approved via admin socket`, and the corresponding
   reject reasons alongside the existing `matched rule …` lines.
10. **Quit.** Click the popover's **Quit Vetter** button (or run
    `systemctl --user stop vetter.service`). The daemon shuts
    down cleanly and the tray icon disappears within ~250 ms.

## Troubleshooting

- **No notification appears.** Confirm a notification daemon is
  running: `gdbus call --session --dest org.freedesktop.Notifications
  --object-path /org/freedesktop/Notifications --method
  org.freedesktop.Notifications.GetServerInformation`. Standard
  Ubuntu Desktop ships `mate-notification-daemon` (XFCE/MATE),
  `notify-osd` (older Unity), or `org.gnome.Shell` (GNOME). If the
  call returns `NameHasNoOwner`, no daemon is on the bus — install
  one (`sudo apt-get install notification-daemon`) or run the
  GNOME / KDE / XFCE shell that bundles its own.
- **Notification appears but Approve / Reject buttons are missing.**
  Some lightweight daemons (`notify-osd`, very old `dunst`) ignore
  the `actions` capability. Confirm with the same `GetCapabilities`
  call: if `actions` is absent, follow the body click-through path
  (PR 2 fallback) or use `vet daemon approve <id>` from a TTY.
- **Tray icon never appears (GNOME).** GNOME 40+ does not ship
  StatusNotifierItem support out of the box. Install
  `sudo apt-get install gnome-shell-extension-appindicator` and
  enable the extension via `gnome-extensions enable
  ubuntu-appindicators@ubuntu.com`. Restart the shell
  (`Alt+F2 → r` on X11; on Wayland log out and back in).
- **Tray icon appears but is invisible against the bar.** The SVG
  is hicolor template-style; the GNOME and KDE themes recolour it
  via `gtk::CssProvider`. If a third-party theme overrides the
  `-symbolic` colour to white-on-white, override
  `~/.config/vetter/icon.css`:

  ```css
  symbol { color: #d8d8d8; }
  ```

- **`vet daemon start` succeeds but no tray icon appears.** As of
  the "default to linux" flip the daemon refuses to come up
  outside a session bus: you should see `vetterd:
  VETTERD_NOTIFIER=linux requires a reachable
  $DBUS_SESSION_BUS_ADDRESS (...)` and a `vet daemon start`
  failure. Either log into a graphical session (the supported
  path) or set `VETTERD_NOTIFIER=noop vet daemon start` to opt
  out of the GUI and rely on `vet daemon list` /
  `vet daemon approve` / `vet daemon reject` for resolution.
- **`systemctl --user status vetter.service` reports
  `condition failed`.** The unit's `ConditionUser=!root` and
  `ConditionEnvironment=DBUS_SESSION_BUS_ADDRESS` guard against
  the headless variants. Ssh into the same UID's GUI session
  (`ssh -X` is not enough — you need a real `gnome-session`) or
  set `VETTERD_NOTIFIER=noop` and run as a regular daemon.
- **Card lists "stuck" pending requests.** A request the daemon
  cannot deliver to the UI (no notification daemon, popover
  closed, no tray host) sits in the pending queue until the user
  resolves it via the popover or `vet daemon approve` / `reject`.
- **`vet` hangs forever.** The notifier callback isn't resolving
  the pending entry; check `journalctl --user -u vetter.service`
  for `dbus call failed` messages and confirm that
  `gdbus monitor --session --dest
  org.freedesktop.Notifications` shows the `ActionInvoked`
  signal arriving when you click Approve / Reject.

## GTK pitfalls we expect to hit

Captured here so future Linux UI work doesn't re-discover them. The
macOS catalogue under [MacOSApp.md](MacOSApp.md) §"AppKit pitfalls
we've hit" lists the AppKit-side equivalents; this section is the
GTK / glib / zbus mirror.

- **`gtk::Application::run` is `Send + !Sync`.** Like AppKit's
  `[NSApp run]`, the GTK main loop owns the calling thread. Spawn
  the accept loop on a background thread first and let
  `vetter::run_with_glib` drive `MainLoop` on the main thread.
  Mirror the macOS observer-thread / `stop_run_loop` pattern via
  `glib::MainContext::default().spawn_local`.
- **Don't share `glib::Object` references across threads.** GObject
  is single-threaded; the queue's change-listener (which fires on
  whatever worker thread resolved the request) must hop back to
  the GTK thread via `glib::idle_add_local` / `glib::MainContext::invoke`
  before touching widgets. Same shape as macOS's
  `MainThreadBound<Retained<AppDelegate>>`.
- **`zbus::Connection` is async-only.** `LinuxNotifier` runs a
  single-threaded `tokio::runtime::Runtime` (or `async-std`) on a
  dedicated thread for the D-Bus client. Do not block the GTK
  main loop on a `zbus` call — the bus daemon can stall under
  heavy load and the popover would freeze. The tested pattern is:
  notifier-thread runs the `zbus` future; signal callbacks
  unblock the `vetterd::pending::PendingQueue::resolve` call,
  which is already thread-safe.
- **`StatusNotifierItem` icon updates require a `NewIcon` signal.**
  Setting the icon name on the `ksni::Tray` struct alone doesn't
  push the new state to the host — call `ksni::Handle::update`
  after every change. The macOS `set_pending_count` analogue is
  one `update` call per queue change-listener fire.
- **Notification capabilities are not stable.** Cache the result
  of `GetCapabilities` at startup, but re-query if the bus
  reports `org.freedesktop.Notifications` going away and coming
  back (notification daemon restart). Same shape as the macOS
  `requestAuthorizationWithOptions_completionHandler` re-query
  on bundle relaunch.

The general rule both platforms argue for: **the GUI code path is
the part of the daemon least covered by integration tests.** Any
new dynamic UI (popover variants, picker sheets, etc.) should be
exercised manually via the smoke test above before merge, *and*
extracted into pure functions wherever feasible so the test suite
can cover the not-GTK half (e.g. queue → card-data lowering, but
not the `gtk::Box::append` assembly).
