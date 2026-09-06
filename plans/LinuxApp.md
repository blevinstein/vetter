# Linux app — status, gaps vs macOS, and the plan to close them

Operational guide and implementation plan for the Linux daemon,
mirroring [MacOSApp.md](MacOSApp.md).

> **Supersedes `plans/UbuntuApp.md`** (deleted 2026-09-06). That
> document described a `.deb` layout, a `[package.metadata.deb]`
> block, a systemd user unit, a `ksni` tray, a GTK4 popover, a
> `VETTERD_NOTIFIER=linux` default, and `vet daemon approve` /
> `vet daemon reject` subcommands. **None of those exist in the
> repo.** It was a design sketch written in the present tense, and
> reading it as an operational guide sends you down paths that fail
> immediately. This document separates *verified today* from
> *planned*, and every "planned" item has a TODO.

For the *visual* design of the approval UI (card layout, signal
pills, host-trust palette, dry-run wrapper, button HIG choices) see
[ApprovalUI.md](ApprovalUI.md). The card catalogue is shared across
platforms; the data layer is unchanged.

---

## 0. TL;DR

The **non-GUI half of vetter already works on Linux**, unmodified.
It builds clean, the full test suite passes (618 tests, 0 failures),
and the daemon's socket / pidfile / audit / XDG path layer is
correct. What is missing is the entire approval surface.

**The one blocking gap:** on Linux there is no way for a human to
approve a prompt-class request. The default notifier is `noop`, and
the admin socket has no resolve verb — `MgmtRequest` exposes
`ListPending`, `SuggestionsFor`, `AddRule`, `RemoveRule`,
`AddKnownHost`, `GetAutostart`, `SetAutostart`, and nothing else.
A prompt-class `vet curl …` on Linux parks forever and only unblocks
(as a deny) when the daemon shuts down and calls `cancel_all`.

Everything else in this plan is UI polish. **Phase 6a below is the
one that turns vetter from unusable to usable on Linux, and it is
about a day of work with no new dependencies.**

---

## 1. Verified on this machine (2026-09-06)

Reference host: Nobara Linux 44 (Fedora 44 base), KDE Plasma 6.7.3,
Wayland session, kernel 7.1.4.

| Check | Result |
|---|---|
| `cargo build --release -p vetterd -p vet` | **OK** — clean, no cfg fallout, ~49 s cold |
| `cargo test --workspace --all-features` | **OK** — 618 passed, 0 failed, 0 ignored |
| `vet doctor` | **OK** — all rows resolve; correct XDG paths |
| `vet daemon start` / `status` / `stop` | **OK** — pidfile, socket, clean teardown, no leftovers |
| Socket path | `/run/user/1000/vetter/vetter.sock`, dir mode 0700, socket 0600 |
| Audit log | `~/.local/state/vetter/audit.log`, mode 0600 |
| `vet curl` → parse → render → policy → park | **OK** — request reaches the pending queue |
| `vet daemon list` | **OK** — shows the parked request with its ULID |
| **Resolve the parked request** | **MISSING — no mechanism exists** |
| `vet doctor` row "code signing" | `SKIP macOS only` — no Linux equivalent yet |
| `vet doctor` row "autostart" | `SKIP macOS only` — no Linux equivalent yet |

Note that `vetter-core::paths` already branches correctly on
`target_os` and honours `$XDG_RUNTIME_DIR` / `$XDG_STATE_HOME`. No
work is needed there.

### Toolchain note

There was no Rust toolchain on this host. Installed via `rustup`
into `~/.cargo` (no sudo, and it is the only installer that honours
the repo's `rust-toolchain.toml`). The distro packages
(`dnf install rust cargo`, 1.98.0) would also satisfy the
workspace's `rust-version = "1.95"`, but a distro `rustc` **ignores
`rust-toolchain.toml`**, so contributor docs should say rustup.

---

## 2. Desktop-environment survey (the substrate we get to build on)

This is the good news. KDE/Plasma is close to a best case for
vetter's UI model — better than the GNOME target the old doc assumed.

| Capability | Probe | Result |
|---|---|---|
| Notification daemon | `GetServerInformation` | `('Plasma', 'KDE', '6.7.3', '1.2')` |
| **Action buttons** | `GetCapabilities` | **`actions` present** — Approve/Reject buttons will render |
| Notification persistence | `GetCapabilities` | `persistence` present — banners survive in history |
| Sound | `GetCapabilities` | `sound` present — the macOS "play sound on new request" setting ports |
| Other | `GetCapabilities` | `body-markup`, `body-hyperlinks`, `inline-reply`, `icon-static`, `x-kde-urls`, `inhibitions` |
| Signal round-trip | `gdbus monitor` + `Notify` | **OK** — `NotificationClosed(id, reason)` observed |
| **Tray host** | `org.kde.StatusNotifierWatcher` | **registered**, `IsStatusNotifierHostRegistered = true` |
| Tray in practice | `RegisteredStatusNotifierItems` | 2 real items already registered — the path is live |
| `systemd --user` | `systemctl --user is-system-running` | `running` |
| XDG portals | `busctl --user list` | `xdg-desktop-portal` + `kde`, `gtk`, `kwallet`, `plasmanotify` backends |
| GTK4 runtime | `rpm -q gtk4` | 4.22.4 installed; `gtk4-devel` available |
| `gtk4-layer-shell` | `dnf list` | 1.3.0 available (escape hatch, see §5.1) |

Contrast with the GNOME assumption baked into the old doc: GNOME 40+
ships **no** StatusNotifierItem host, which is why that doc had a
troubleshooting entry telling users to install
`gnome-shell-extension-appindicator`. On KDE the tray Just Works.
That asymmetry is real and belongs in the per-desktop deltas (§9),
not in the main flow.

---

## 3. Gap analysis vs macOS

macOS is the gold standard. Here is the full surface, and where
Linux stands.

### 3.1 Portable today — no work needed

| Feature | Notes |
|---|---|
| curl parser, renderer, risk analyzer | Pure Rust, no platform code |
| Layered allowlist + matcher, session/expiring rules | `peer_cred::stable_session_for` already has a native Linux impl (`getsid`/ppid/tty, no `ps`) |
| Suggestion engine (allowlist tiers, known-host tiers) | Pure Rust |
| Daemon socket IPC, pending queue, inflight cap | Portable |
| Admin socket (`ListPending`, `AddRule`, `RemoveRule`, `AddKnownHost`, `SuggestionsFor`) | Portable, verified working |
| Audit log | Portable, correct XDG path |
| Pidfile + peer-uid attestation | `pidfile.rs` already has a `target_os = "linux"` branch |
| `vet doctor` (11 of 13 rows) | Portable |

### 3.2 Missing on Linux

| macOS feature | Implementation | Linux status |
|---|---|---|
| **Resolve a prompt** | UI callbacks → `PendingQueue::resolve` | **MISSING — nothing can resolve** |
| Notification with Approve/Reject | `UNUserNotificationCenter` (`notifier/mac.rs`) | **MISSING** — no `LinuxNotifier` |
| Notification body click-through → detail UI | `runloop/mod.rs` | **MISSING** |
| Banner coalescing (`NotifyHint::was_empty_before`) | `notifier/mac.rs` | Hint plumbing is portable; **no consumer** |
| Banner dismissal on resolve | `removeDeliveredNotificationsWithIdentifiers:` | **MISSING** (`CloseNotification` is the analogue) |
| Notification sound setting | `UNNotificationSound::defaultSound()` | **MISSING** (`sound` hint is the analogue) |
| Tray icon + pending-count badge | `runloop/status_item.rs` (180 ln) | **MISSING** — no SNI item |
| Approval window listing pending cards | `runloop/popover.rs` (2250 ln) + 5 helper modules (~1500 ln) | **MISSING** |
| — §8.5 detail disclosure | `toggleDetailsDisclosure:` | **MISSING** |
| — raw-command disclosure + copy | `toggleRawDisclosure:`, `copyRawClicked:` | **MISSING** |
| — signal pills | `popover_pills.rs` | **MISSING** |
| — URL styling | `popover_url.rs` | **MISSING** |
| — `Allowlist…` picker (with duration radios) | `popover_picker.rs` (760 ln) | **MISSING** |
| — `Trust host…` picker | `popover_picker.rs` | **MISSING** |
| — `See approval reason` + `Revoke rule` | `revokeRuleClicked:` | **MISSING** (wire verb exists) |
| — `Open file` button on FileRead rows | `openFileClicked:` → `NSWorkspace` | **MISSING** (`xdg-open` is the analogue) |
| — Quit button | `requestShutdown:` | **MISSING** |
| Autostart on login | `SMAppService.mainApp` (`autostart.rs`) | **STUB** — returns `Unsupported` on non-macOS |
| — `vet daemon autostart enable/disable/status` | Wire verbs exist and are portable | Works, but always errors on Linux |
| — `vet doctor` autostart row | `doctor.rs` | Hard-coded `SKIP macOS only` |
| Code-signing verification | `codesign --verify` | Hard-coded `SKIP macOS only`; Linux analogue is RPM/GPG signature |
| Packaged install | Homebrew cask, notarised bundle | **MISSING** |
| Process/UI identity | `.app` bundle + `LSUIElement` + Info.plist | Analogue is a `.desktop` entry (see §5.2) |

### 3.3 Documented-but-nonexistent (traps in the current docs)

These appear in `AGENTS.md`, `plans/Overview.md`, `plans/Release.md`,
`plans/TestingPlan.md`, and the deleted `UbuntuApp.md` as if they
ship. They do not exist:

- `vet daemon approve <id>` / `vet daemon reject <id>` — **the
  documented headless escape hatch on every platform, including
  macOS.** Not implemented. `vet daemon --help` lists only `start`,
  `stop`, `status`, `list`, `autostart`.
- `VETTERD_NOTIFIER=linux` — not a valid value; `build_from_env`
  accepts only `mac` / `mock` / `noop`. The claim that the daemon
  "refuses to install if `$DBUS_SESSION_BUS_ADDRESS` is unset and
  exits 78" is fiction; on Linux it silently comes up with `noop`.
- `[package.metadata.deb]` in `vetterd/Cargo.toml` — absent.
- `/usr/lib/systemd/user/vetter.service` — no unit file in the repo.
- `/etc/xdg/autostart/vetter.desktop` — no desktop entry in the repo.
- `dev.vetter.daemon.svg` tray icon — `assets/` holds only
  `vetter-logo.{png,svg}`; no hicolor-scalable install path.
- `tools/release-deb.sh` — referenced by `Release.md:15`; `tools/`
  contains only the macOS scripts.
- `vetterd/src/runloop/linux/` — referenced by `Overview.md:439`
  and `:843`; does not exist.
- `~/.config/vetter/icon.css` — a `gtk::CssProvider` override path
  for an icon that doesn't exist.

See §10 for what to do about the spec documents.

---

## 4. Decisions taken (2026-09-06)

Recorded so the phases below don't get re-litigated.

1. **One `plans/LinuxApp.md`, replacing `UbuntuApp.md`.** ~95% of
   the work is desktop- and distro-agnostic. Nobara/KDE/Wayland is
   the primary development target; Ubuntu/GNOME deltas live in §9.
2. **Approval UI = notification actions + a plain top-level
   window.** Not an anchored popover — see §5.1 for why that is not
   achievable on Wayland. The tray icon opens a normal,
   compositor-placed window listing the pending cards. This ships
   the full §8.5 card catalogue and loses only the
   anchored-to-the-menu-bar feel.
3. **Install mechanism = release tarball + `cargo install` for
   now.** No COPR, no Flatpak, no PPA in this milestone. Prebuilt
   `x86_64-unknown-linux-gnu` tarballs on GitHub Releases plus a
   documented `cargo install --git` path. Native packaging is a
   follow-up once the UI has settled (§8).

---

## 5. Linux/Wayland constraints that change the macOS design

These are the places where a straight port of the AppKit design does
not work. Worth internalising before writing any UI code.

### 5.1 There is no anchored popover on Wayland

macOS's `NSPopover` `showRelativeToRect:ofView:preferredEdge:` anchors
the approval UI to the menu-bar status item. **Wayland has no
equivalent and cannot have one.** A Wayland client cannot position a
surface at absolute screen coordinates, and `xdg_positioner` anchors
only against *the client's own* surfaces. Our tray icon is not our
surface — it is drawn by `plasmashell` from the `StatusNotifierItem`
properties we publish over D-Bus. There is no protocol by which
plasmashell tells us where it drew our icon.

The three real options, and why we chose (a):

- **(a) A plain `gtk::ApplicationWindow`** the compositor places.
  Chosen. Full card catalogue, no exotic protocols, works on
  X11 and Wayland, works on GNOME and KDE.
- **(b) An `SNI` + `com.canonical.dbusmenu` menu.** Fully
  Wayland-native and the host positions it correctly next to the
  icon. But DBusMenu is a *menu* — flat items, labels, checkmarks,
  submenus. The §8.5 detail cards, signal pills, disclosure
  triangles, and picker sheets have no representation. Good enough
  for `Approve` / `Reject` / `Open Vetter…` / `Quit`, which is
  exactly what we should put in it as a *secondary* surface.
- **(c) `gtk4-layer-shell`.** Places a surface at a screen edge as
  an overlay. Closer to the macOS feel, but it is a
  compositor-specific protocol (`zwlr_layer_shell_v1`), still can't
  track the tray icon, and adds a hard dependency. Rejected.

**Implication for `ApprovalUI.md`:** the Linux card layout is a
window, not a popover. Card contents are unchanged; the container,
its sizing, and its dismissal semantics differ. Worth an explicit
note in that document.

### 5.2 There is no bundle identity

`MacNotifier::install` refuses to start unless
`is_app_bundle_executable(current_exe())` — the `.app` layout is
load-bearing on macOS for `LSEnvironment`, notification-centre
identity, and code-signing. Linux has no analogue. The nearest thing
is a **`.desktop` entry** whose basename matches the `desktop-entry`
hint we pass to `Notify`, which is what makes the notification show
"Vetter" with our icon instead of a generic placeholder. That is
cosmetic, not a security boundary, so the Linux notifier must **not**
inherit the macOS "refuse to start outside a bundle" guard.

The equivalent Linux precondition worth guarding on is a **reachable
session bus** (`$DBUS_SESSION_BUS_ADDRESS`), which is what the old
doc claimed the code already checked. Implement it for real.

### 5.3 Autostart is a file, not an API

`SMAppService.mainApp` has no counterpart. Linux autostart is
`~/.config/autostart/vetter.desktop` (XDG autostart spec), or a
`systemd --user` unit with `WantedBy=default.target`. Both are
files we write, which means:

- `autostart::current()` becomes a filesystem check, not an API
  query — cheap, so the "safe to call on every window open"
  property holds.
- There is no `RequiresApproval` state to model, so the macOS
  rollback-on-error UI path simplifies.
- The XDG `.desktop` route is desktop-agnostic and works on KDE,
  GNOME, XFCE. The systemd route is cleaner for a daemon but is
  ignored by desktops that don't wire `graphical-session.target`
  properly. **Recommend XDG autostart** as the primary, with the
  systemd unit as an optional extra for packaged installs.

### 5.4 Notification capabilities are not guaranteed

Plasma advertises `actions`. Many daemons do not (`notify-osd`, old
`dunst`, some minimal WM setups). The macOS design can assume its
buttons render; the Linux one cannot. Cache `GetCapabilities` at
startup, **and degrade**: if `actions` is absent, fall back to a
body-click-through that opens the window. Re-query when
`org.freedesktop.Notifications` drops off and returns to the bus
(daemon restart) — `NameOwnerChanged` is the signal.

### 5.5 The tray host is not guaranteed either

KDE registers `org.kde.StatusNotifierWatcher` natively. GNOME does
not, without an extension. The daemon must come up and be fully
usable with no tray at all — so the window needs an addressable
entry point that isn't "click the tray icon". Options: a
`vet daemon open` subcommand, and/or activating an existing instance
via a well-known D-Bus name.

### 5.6 Focus stealing / activation tokens

On Wayland a client cannot raise its own window unprompted. Opening
the approval window in response to a notification click needs the
**activation token** the notification daemon hands over (Plasma emits
`ActivationToken(id, token)` alongside `ActionInvoked`; the XDG spec
also defines the `activation-token` hint). Without it the window may
open unfocused or merely flag the taskbar. Handle the signal.

### 5.7 `xdg-open`, not `NSWorkspace`

The popover's "Open file" button maps to `xdg-open <path>`. Same
suppression rule (hide the button when the path does not exist).

---

## 6. Phased plan

### Phase 6a — Headless resolve path `[ ]` **← start here**

This is the unblocker, and it is platform-independent: **macOS is
missing it too.** It costs no new dependencies and makes vetter
immediately usable on Linux from a terminal, well before any GUI
lands. It also gives every later phase a resolve path to test
against.

- [ ] Add `MgmtRequest::Resolve { id: String, decision: …, reason: Option<String> }`
      to [vetter-core/src/wire/mod.rs](../vetter-core/src/wire/mod.rs),
      plus the matching `MgmtResponse`. Reuse the existing
      allow/deny decision type rather than inventing a third.
- [ ] Handle it in the daemon's admin loop
      ([vetterd/src/lib.rs](../vetterd/src/lib.rs)) by calling
      `PendingQueue::resolve`, exactly as the macOS UI callbacks do.
      Audit reason: `"approved via admin socket"` /
      `"rejected via admin socket"` — the strings the docs already
      promise.
- [ ] `vet daemon approve <id>` / `vet daemon reject [--reason R] <id>`
      in [vet/src/daemon.rs](../vet/src/daemon.rs). Accept a unique
      ULID prefix, not just the full 26 chars — the ids are long and
      this is a hand-typed command.
- [ ] Decide and document whether an unknown / already-resolved id
      is an error or a no-op (recommend: error, exit non-zero).
- [ ] Integration coverage in
      [vetterd/tests/admin_ipc.rs](../vetterd/tests/admin_ipc.rs):
      approve unblocks the parked client with exit 0; reject
      unblocks with 77; both write the right audit reason;
      double-resolve is rejected.
- [ ] Confirm this is reachable over SSH with no `$DISPLAY` /
      `$DBUS_SESSION_BUS_ADDRESS` — that is the documented headless
      story and it should finally be true.

### Phase 6b — `LinuxNotifier` (D-Bus notifications) `[ ]`

- [ ] Add `zbus` (5.19) under
      `[target.'cfg(target_os = "linux")'.dependencies]` in
      [vetterd/Cargo.toml](../vetterd/Cargo.toml), mirroring how the
      `objc2` stack is gated for macOS.
- [ ] `vetterd/src/notifier/linux.rs` implementing `Notifier`:
      `Notify` with `actions = ["approve", "Approve", "reject", "Reject"]`,
      `desktop-entry` hint, `urgency = critical`, `expire_timeout = 0`
      (never auto-expire — a prompt must not silently vanish).
- [ ] Subscribe to `ActionInvoked`, `NotificationClosed`, and
      `ActivationToken`; map notification id → request ULID;
      route into `PendingQueue::resolve`.
- [ ] `CloseNotification` on every resolve path — including
      resolves that came from the admin socket or a rule addition
      (the macOS `persist_rule_async` analogue), so banners don't
      linger.
- [ ] Consume `NotifyHint::was_empty_before` for coalescing (spec
      §7: "we don't spam banners"). The hint is already computed.
- [ ] Cache `GetCapabilities`; re-query on `NameOwnerChanged` for
      `org.freedesktop.Notifications` (§5.4). Degrade to
      click-through when `actions` is absent.
- [ ] Honour `settings.notification_sound` via the `sound-name` hint.
- [ ] Add `"linux"` to `notifier::build_from_env` and make it the
      default `default_kind()` on `target_os = "linux"`. Guard on a
      reachable session bus, **not** on any bundle notion (§5.2);
      exit 78 with a message pointing at `VETTERD_NOTIFIER=noop`.
- [ ] Threading: run the `zbus` connection on its own thread with a
      single-threaded async runtime. Never block the UI thread on a
      bus call.

At the end of 6b, Linux has a working approve/reject loop for the
common case, without any GUI toolkit dependency at all. **This is a
credible v0.2 ship point.**

### Phase 6c — Tray icon (StatusNotifierItem) `[ ]`

- [ ] Evaluate `ksni` (0.3.6) vs. hand-rolling the SNI object on
      the `zbus` connection we already have in 6b. Hand-rolling
      avoids a second D-Bus stack and a possible zbus-version
      conflict — check `ksni`'s zbus dependency before committing.
- [ ] Publish `StatusNotifierItem` with the shield icon; register
      with `org.kde.StatusNotifierWatcher`, falling back to
      `org.freedesktop.StatusNotifierWatcher`.
- [ ] Pending-count badge. Note the SNI analogue of the macOS badge
      is either `IconName`/`IconPixmap` swapping, an `OverlayIcon`,
      or `ToolTip` text — Plasma renders overlay icons, GNOME's
      extension may not. Emit `NewIcon` / `NewToolTip` after every
      change; setting properties alone does not push state.
- [ ] Wire the queue's change-listener to update the badge (macOS's
      `set_pending_count` analogue).
- [ ] `com.canonical.dbusmenu` context menu: **Open Vetter…**,
      **Pending: N**, **Quit Vetter** — plus per-request
      Approve/Reject items, which is genuinely useful on Wayland
      because the host positions the menu correctly (§5.1b).
- [ ] Ship the icon: generate a symbolic/scalable SVG from
      `assets/vetter-logo.svg` into
      `share/icons/hicolor/scalable/apps/dev.vetter.daemon.svg`,
      and a `dev.vetter.daemon.desktop` entry (needed for the
      `desktop-entry` notification hint anyway, §5.2).
- [ ] Daemon must start and stay useful when no watcher is present
      (§5.5).

### Phase 6d — Approval window (GTK4) `[ ]`

The big one. Roughly the Linux counterpart of ~3700 lines of AppKit.

- [ ] Add `gtk4` (0.11) gated to `target_os = "linux"`. Document
      the build deps (`gtk4-devel glib2-devel` on Fedora;
      `libgtk-4-dev libglib2.0-dev` on Debian/Ubuntu).
- [ ] Introduce `PlatformDriver::Gtk` alongside `None` / `AppKit`
      in [vetterd/src/lib.rs](../vetterd/src/lib.rs); `run_with_glib`
      drives the main loop on the main thread with the accept loop
      on a background thread, mirroring `run_with_appkit`.
- [ ] `vetterd/src/runloop/linux/` — window + card assembly. Follow
      the macOS split: keep the pure lowering functions
      (queue → card data, effects → rows, signals → pills, URL
      segmentation) **shared and unit-tested**, and let the
      GTK module own only widget assembly. Several of the existing
      macOS helpers (`popover_url.rs`, `popover_pills.rs`,
      `popover_effects.rs`, `popover_attr.rs`) are currently
      `#![cfg(target_os = "macos")]` but are substantially
      platform-independent logic — **lifting the pure half out is a
      prerequisite, not an afterthought.**
- [ ] Cards: §8.5 detail disclosure, raw-command disclosure + copy,
      signal pills, host-trust palette, `Open file` via `xdg-open`.
- [ ] Per-card **Approve** / **Reject**.
- [ ] `Allowlist…` and `Trust host…` pickers including the duration
      radio group (15m / 1h / 4h / this terminal session / Forever).
      These drive `AddRule` / `AddKnownHost`, which already work.
- [ ] `See approval reason` + **Revoke rule** on auto-allow Recent
      cards, driving the existing `RemoveRule`.
- [ ] Footer: **Quit Vetter**, **Start at login**, **Play sound on
      new request**.
- [ ] Notification click-through opens the window scrolled to the
      matching card, using the activation token (§5.6).
- [ ] Single-instance / raise-existing entry point (§5.5).
- [ ] Threading discipline: GObject is single-threaded. The queue's
      change-listener fires on whatever worker thread resolved the
      request and must hop to the GTK thread via
      `glib::idle_add_local` / `MainContext::invoke` before touching
      widgets. This is the macOS `MainThreadBound` pattern.

### Phase 6e — Autostart + `vet doctor` parity `[ ]`

- [ ] Linux `autostart::sys` writing/removing
      `~/.config/autostart/vetter.desktop` (§5.3); `current()` is a
      file-existence + `Hidden=` check.
- [ ] `reconcile_with_settings` then works unchanged on Linux — it
      already no-ops only on `Unsupported`.
- [ ] `vet doctor` autostart row: drop the `cfg!(target_os = "macos")`
      early return, report the real state.
- [ ] `vet doctor` "code signing" row: on Linux report package
      provenance instead (`rpm -V` / dpkg verify when installed from
      a package; `INFO built from source` otherwise), or keep the
      SKIP with an honest reason.
- [ ] New Linux-only doctor rows worth having, given §5.4/§5.5:
      session bus reachable; notification daemon present +
      `actions` capability; StatusNotifierWatcher present;
      `$XDG_RUNTIME_DIR` sane.

### Phase 6f — Distribution `[ ]`

Per §4.3: tarball + `cargo install` only, for now.

- [ ] `tools/build-linux.sh` producing a staged tree
      (`bin/vet`, `bin/vetterd`, `share/icons/…`,
      `share/applications/dev.vetter.daemon.desktop`) and a
      `vetter-<version>-x86_64-linux.tar.gz`.
- [ ] An `install.sh` inside the tarball that copies into
      `~/.local` (no sudo) and refreshes the icon cache + desktop
      database.
- [ ] Document `cargo install --git https://github.com/blevinstein/vetter vet vetterd`
      as the from-source path, with the per-distro build-dep list.
- [ ] Attach the tarball to GitHub Releases alongside the macOS
      artifacts.
- [ ] **Update `README.md`**: it currently promises
      `sudo add-apt-repository ppa:blevinstein/vetter` under
      "Ubuntu (v0.2, planned)". Replace with the tarball /
      `cargo install` instructions and drop the PPA promise until
      §8 is decided.

### Phase 6g — CI `[ ]`

- [ ] Linux job already runs fmt/clippy/tests. Extend it to build
      the new `target_os = "linux"` cfg paths — otherwise the
      notifier and GTK code never get compiled in CI.
- [ ] Install `gtk4-devel` / `libgtk-4-dev` in the Linux job.
- [ ] Keep the `MockNotifier` as the automated-coverage workhorse;
      CI has no session bus, so 6b/6c/6d get manual smoke coverage
      only (§7) plus unit tests on the extracted pure functions.
- [ ] Consider a `dbus-run-session` + `dunst` job to smoke the
      `LinuxNotifier` for real. `dunst` supports `actions`, so an
      end-to-end approve could genuinely be automated. Worth
      prototyping — this would be better coverage than macOS has.

---

## 7. Manual smoke test (Linux) — **target state, not yet runnable**

Every step below depends on phases that are not written yet. Marked
with the phase that unlocks it. Until 6a lands, only steps 1–3 and
the headless step work.

1. Build: `cargo build --release -p vetterd -p vet`, then
   `export PATH="$PWD/target/release:$PATH"`. *(works today)*
2. `vet daemon start`; `vet doctor` should be all OK/INFO.
   *(works today)*
3. `vet curl https://prompt-test.example/` parks; `vet daemon list`
   shows it with a ULID. *(works today)*
4. **Headless resolve.** `vet daemon approve <id>` → the parked
   `vet` exec's curl and returns curl's exit code.
   `vet daemon reject <id>` → exit 77. Works over SSH with no
   `$DISPLAY`. *(6a)*
5. **Notification path.** A Plasma notification appears with
   **Approve** and **Reject**. Clicking either resolves the request
   and the banner clears. *(6b)*
6. **Coalescing.** Two concurrent `vet curl` commands raise only one
   banner; the second coalesces. *(6b)*
7. **Capability degradation.** Repeat under a daemon without
   `actions` (e.g. `notify-osd`); confirm the body-click path still
   reaches the UI. *(6b)*
8. **Tray.** A shield appears in the Plasma system tray. Right-click
   → **Open Vetter…**, **Pending: N**, **Quit Vetter**. Badge
   tracks the pending count. *(6c)*
9. **Window.** Clicking the tray icon opens the approval window with
   one card per pending request, §8.5 detail, signal pills, and
   per-card Approve / Reject. *(6d)*
10. **Click-through.** Click the notification *body*; the window
    opens focused (activation token) with the matching card scrolled
    into view; `vet` is still blocked. *(6d)*
11. **Pickers.** `Allowlist…` with a 15m duration; confirm a
    matching later request auto-approves and that the rule expires.
    `Trust host…` flips the host pill from unknown to known. *(6d)*
12. **Revoke.** On an auto-allowed Recent card, `See approval
    reason` names the rule + scope; **Revoke rule** removes it after
    confirmation. *(6d)*
13. **Audit log.** `tail -n8 ~/.local/state/vetter/audit.log` shows
    a mix of `approved via notification`, `approved via window`,
    `approved via admin socket`, and `matched rule …`. *(6a–6d)*
14. **Autostart.** Tick **Start at login**; confirm
    `~/.config/autostart/vetter.desktop` appears; log out and back
    in; the tray returns. *(6e)*
15. **Quit.** **Quit Vetter** (or `vet daemon stop`) shuts down
    cleanly; tray icon disappears within ~250 ms; socket and pidfile
    are removed. *(6c/6d; `vet daemon stop` works today)*

---

## 8. Open investigations

- **Packaging, revisited.** The §4.3 decision defers this. When it
  comes back up, the candidates are **Fedora COPR** (natural
  counterpart to the homebrew tap; `rpmbuild` is already on the dev
  box; matches this machine), **Ubuntu PPA** (what README currently
  promises; needs a Launchpad account and an Ubuntu build host), and
  **Flatpak**. Flatpak deserves a real look *and* a real
  skepticism: vetter must `execvp` arbitrary host binaries (`curl`)
  and bind a socket in `$XDG_RUNTIME_DIR`. Under a Flatpak sandbox
  the wrapped command would run inside the sandbox, not on the
  host — which likely breaks the entire premise. Investigate before
  promising it anywhere.
- **`ksni` vs. hand-rolled SNI.** Check `ksni` 0.3.6's zbus
  dependency against the zbus 5.x we'd pull in for 6b. Two D-Bus
  stacks in one process is a smell.
- **How much of `popover_*.rs` is actually AppKit?** Several of
  those modules are `#![cfg(target_os = "macos")]` at the file level
  but contain mostly pure logic. Auditing and lifting the pure half
  (with its existing unit tests) is prerequisite work for 6d and
  would shrink that phase materially. Do this measurement before
  estimating 6d.
- **Does `vet` need to know the daemon has no UI?** Today a
  prompt-class request under `VETTERD_NOTIFIER=noop` parks silently
  and forever. Should `vet` print "waiting for approval — run
  `vet daemon approve <id>`" to stderr after N seconds? That would
  be a large usability win on Linux and costs nothing on macOS.
  (Careful: stderr only — `vet` has a byte-perfect stdout
  passthrough invariant.)
- **Pending-request timeout.** There is none. A parked request holds
  an inflight slot indefinitely. On macOS the UI guarantees a human
  sees it; on Linux with no notification daemon nothing does.
  Consider a configurable deadline that fails closed.
- **X11 sessions.** Everything above is validated on Wayland only.
  X11 is strictly easier (anchored windows are possible) but should
  still be smoke-tested; don't let the Wayland-first design regress
  X11.
- **Multi-seat / multi-session.** `$XDG_RUNTIME_DIR` is per-user,
  not per-session. Two concurrent graphical logins for the same UID
  would contend for one socket and one daemon. macOS has the same
  shape; worth confirming the behaviour is sane rather than
  corrupting.

---

## 9. Per-desktop / per-distro deltas

The plan targets the common substrate. Known divergences:

| | KDE Plasma (reference) | GNOME | XFCE / others |
|---|---|---|---|
| Tray host | Native, always present | **Absent** without `gnome-shell-extension-appindicator` | Usually native |
| Notification `actions` | Yes | Yes | Varies (`notify-osd` no; `dunst` yes) |
| Notification persistence | Yes | Yes | Varies |
| Portal backend | `kde` | `gtk` | `gtk` |
| GTK4 look | Foreign but functional | Native | Foreign |

| | Fedora / Nobara (reference) | Debian / Ubuntu |
|---|---|---|
| Build deps | `gtk4-devel glib2-devel dbus-devel` | `libgtk-4-dev libglib2.0-dev libdbus-1-dev` |
| Rust | `dnf install rust cargo` (ignores `rust-toolchain.toml`) — prefer rustup | same caveat |
| Packaging (deferred) | COPR / `.rpm` | PPA / `.deb` |

**GNOME's missing tray host is the single biggest portability risk**
in Phase 6c, and it is exactly the case §5.5 exists to handle: the
daemon must be fully usable with the notification path plus a
non-tray entry point into the window.

---

## 10. Spec reconciliation

`AGENTS.md` says not to edit `plans/Overview.md` or
`plans/TestingPlan.md` to fit the implementation — raise
discrepancies instead. Raising them here:

- **`Overview.md` §6 "Linux (Ubuntu 22.04+)" and §11 Phase 6**
  describe a `.deb` + systemd + `VETTERD_NOTIFIER=linux` +
  `vetterd/src/runloop/linux/` design. The architecture is sound and
  this plan follows it; the specifics that need a spec decision are
  (a) the popover→window change forced by Wayland (§5.1), (b) the
  bundle-guard removal (§5.2), and (c) the packaging deferral (§4.3).
- **`Overview.md`** should also record that `vet daemon approve` /
  `reject` — referenced across the docs as the headless escape
  hatch — is unimplemented on **every** platform, and that Phase 6a
  is where it lands.
- **`Release.md` §"Linux / Launchpad PPA"** documents a Launchpad
  flow and a `tools/release-deb.sh` that does not exist. Under §4.3
  this section is on hold; it should be marked as such rather than
  read as current.
- **`TestingPlan.md`:690** points at the old `UbuntuApp.md` smoke
  procedure. Repoint at §7 here, and note that steps are gated on
  unlanded phases.
- **`ApprovalUI.md`** should gain a note that the Linux container is
  a window, not a popover (§5.1).
- **`README.md`** promises a PPA (see 6f).
- Mechanical link updates from `UbuntuApp.md` → `LinuxApp.md` have
  been made in `AGENTS.md`, `TODO.md`, `RepoMap.md`, `Release.md`,
  `Overview.md`, and `TestingPlan.md`. No prose in the two spec
  documents was otherwise touched.

---

## 11. GTK / glib / zbus pitfalls

Carried over from the old document — this section was the part
worth keeping — with corrections. The macOS catalogue under
[MacOSApp.md](MacOSApp.md) §"AppKit pitfalls we've hit" is the
counterpart.

- **`gtk::Application::run` owns the calling thread.** Like
  `[NSApp run]`. Spawn the accept loop on a background thread first
  and drive the `MainLoop` on the main thread. Mirror the macOS
  observer-thread / `stop_run_loop` pattern via
  `glib::MainContext::default().spawn_local`.
- **Don't share `glib::Object` across threads.** GObject is
  single-threaded. The queue's change-listener fires on whatever
  worker thread resolved the request and must hop back via
  `glib::idle_add_local` / `glib::MainContext::invoke` before
  touching widgets. Same shape as macOS's
  `MainThreadBound<Retained<AppDelegate>>`.
- **`zbus::Connection` is async-only.** Run it on a dedicated
  thread with a single-threaded runtime. Never block the GTK main
  loop on a bus call — the bus can stall under load and the window
  would freeze. `PendingQueue::resolve` is already thread-safe, so
  signal callbacks can call it directly from the bus thread.
- **`StatusNotifierItem` updates need an explicit signal.** Setting
  the icon property does not push state to the host; emit `NewIcon`
  / `NewAttentionIcon` / `NewToolTip`. One emission per queue
  change-listener fire.
- **Notification capabilities are not stable.** Cache
  `GetCapabilities` at startup but re-query on `NameOwnerChanged`
  for `org.freedesktop.Notifications` (§5.4).
- **`expire_timeout = 0` means never expire; `-1` means "daemon
  decides".** A prompt must use `0` — a silently vanishing approval
  request is a correctness bug, not a cosmetic one.
- **Notification ids are `u32` assigned by the daemon and reused
  across daemon restarts.** Don't use them as the primary key for a
  request; keep a bidirectional map to our ULIDs and rebuild it if
  the daemon disappears from the bus.

The general rule both platforms argue for: **the GUI path is the
least-covered part of the daemon.** Extract the not-GTK half (queue
→ card data lowering) into pure, unit-tested functions, and exercise
the widget assembly manually via §7 before merge.
