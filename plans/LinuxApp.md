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
It builds clean, the full test suite passes (863 tests, 0 failures),
and the daemon's socket / pidfile / audit / XDG path layer is
correct. The approval surface — notifier, tray, window, pickers,
autostart, doctor rows — was missing when this was written and
landed across 6a–6e, 6h and 6i. What remains is distribution (§6f)
and one CI job (§6g).

**The one blocking gap — closed by Phase 6a on 2026-09-06.** It used
to be that on Linux nothing could approve a prompt-class request: the
default notifier is `noop` and the admin socket had no resolve verb,
so a prompt-class `vet curl …` parked forever and only unblocked (as
a deny) when the daemon shut down and called `cancel_all`.
`MgmtRequest::Resolve` and `vet daemon approve` / `vet daemon reject`
now close that loop, on every platform, with no new dependencies and
no GUI — see §6a.

Everything else in this plan was UI, and is now built.

---

## 1. Verified on this machine (2026-09-06)

Reference host: Nobara Linux 44 (Fedora 44 base), KDE Plasma 6.7.3,
Wayland session, kernel 7.1.4.

| Check | Result |
|---|---|
| `cargo build --release -p vetterd -p vet` | **OK** — clean, no cfg fallout, ~49 s cold |
| `cargo test --workspace --all-features` | **OK** — 863 passed, 0 failed, 0 ignored (2026-09-09) |
| `cargo clippy --workspace --all-targets` | **OK** — both feature configurations; was red on two unused `PermissionsExt` imports until 2026-09-06 (TODO.md §"Build health") |
| `vet doctor` | **OK** — all rows resolve; correct XDG paths |
| `vet daemon start` / `status` / `stop` | **OK** — pidfile, socket, clean teardown, no leftovers |
| Socket path | `/run/user/1000/vetter/vetter.sock`, dir mode 0700, socket 0600 |
| Audit log | `~/.local/state/vetter/audit.log`, mode 0600 |
| `vet curl` → parse → render → policy → park | **OK** — request reaches the pending queue |
| `vet daemon list` | **OK** — shows the parked request with its ULID |
| **Resolve the parked request** | **OK** — `vet daemon approve` / `reject` (Phase 6a, landed 2026-09-06) |
| `vet doctor` row "code signing" | **OK** — became the `provenance` row on Linux (Phase 6e, 2026-09-09); reports package ownership, or "built from source" |
| `vet doctor` row "autostart" | **OK** — reports real state from `~/.config/autostart/vetter.desktop` (Phase 6e, 2026-09-09) |

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
| **Resolve a prompt** | UI callbacks → `PendingQueue::resolve` | **DONE (6a)** — `MgmtRequest::Resolve` + `vet daemon approve`/`reject` |
| Notification with Approve/Reject | `UNUserNotificationCenter` (`notifier/mac.rs`) | **DONE (6b)** — `notifier/linux.rs` over `org.freedesktop.Notifications` |
| — notification `Allowlist…` / `Trust host…` actions | `runloop/mod.rs` registers **four** actions, plus a second category so `Trust host…` shows only for unknown hosts | **DONE (6i)** — same four, gated the same way |
| Notification body click-through → detail UI | `runloop/mod.rs` | **DONE (6d step 4)** — `default` action → window, scrolled to the card |
| Banner coalescing (`NotifyHint::was_empty_before`) | `notifier/mac.rs` | **DONE (6b)** — consumed by `notifier/linux.rs` |
| Banner dismissal on resolve | `removeDeliveredNotificationsWithIdentifiers:` | **DONE (6b)** — `CloseNotification`, driven off the queue change listener so *every* resolve path closes |
| Notification sound setting | `UNNotificationSound::defaultSound()` | **DONE (6b)** — `sound-name` hint |
| Tray icon + pending-count badge | `runloop/status_item.rs` (180 ln) | **DONE (6c)** — `ksni` SNI item in `tray.rs`; badge is `ToolTip` + `NeedsAttention`, not an overlay icon (GNOME's extension does not render those reliably) |
| Approval window listing pending cards | `runloop/popover.rs` (2250 ln) + 5 helper modules (~1500 ln) | **DONE (6d)** — `runloop/linux/`, a plain window rather than an anchored popover (§5.1) |
| — §8.5 detail disclosure | `toggleDetailsDisclosure:` | **DONE (6d step 2)** — structured rows inline on pending cards; the disclosure is Recent-card behaviour |
| — raw-command disclosure + copy | `toggleRawDisclosure:`, `copyRawClicked:` | **DONE (6d step 2)** |
| — signal pills | `popover_pills.rs` | **DONE (6d step 2)** — via the shared `cards::pills` lowering |
| — URL styling | `popover_url.rs` | **DONE (6d step 2)** — via the shared `cards::url` lowering |
| — `Allowlist…` picker (with duration radios) | `popover_picker.rs` (760 ln) | **DONE (6d step 3)** |
| — `Trust host…` picker | `popover_picker.rs` | **DONE (6d step 3)** |
| — `See approval reason` + `Revoke rule` | `revokeRuleClicked:` | **DONE (6d step 3)** |
| — `Open file` button on FileRead rows | `openFileClicked:` → `NSWorkspace` | **DONE (6d step 2)** — `xdg-open`, suppressed when the path does not exist, same rule as macOS |
| — Quit button | `requestShutdown:` | **DONE (6d step 3)** — window footer, plus the tray's Quit; both flip the shutdown flag rather than exiting from a callback |
| Autostart on login | `SMAppService.mainApp` (`autostart.rs`) | **DONE (6e)** — XDG autostart entry; `NotFound` when the pinned `Exec` has moved |
| — `vet daemon autostart enable/disable/status` | Wire verbs exist and are portable | **DONE (6e)** — per-platform wording so Linux is not told about "login items" |
| — `vet doctor` autostart row | `doctor.rs` | **DONE (6e)** — reports real state; macOS strings unchanged |
| Code-signing verification | `codesign --verify` | **DONE (6e)** — replaced by a `provenance` row (package ownership, not integrity; see §6e) |
| Packaged install | Homebrew cask, notarised bundle | **MISSING** — the one remaining gap; tarball + `cargo install` per §4.3, tracked in §6f |
| Process/UI identity | `.app` bundle + `LSUIElement` + Info.plist | Analogue is a `.desktop` entry (see §5.2) |

### 3.3 Documented-but-nonexistent (traps in the current docs)

These appeared in `AGENTS.md`, `plans/Overview.md`,
`plans/Release.md`, `plans/TestingPlan.md`, and the deleted
`UbuntuApp.md` as if they shipped, when none of them existed.
Struck-through entries have since been built; the rest are still
absent, and several are absent *by decision* rather than by
omission — which is the more useful thing to record:

- ~~`vet daemon approve <id>` / `vet daemon reject <id>`~~ — was
  **the documented headless escape hatch on every platform,
  including macOS**, and was not implemented. **Landed in Phase 6a
  (2026-09-06)**; `vet daemon --help` now lists `start`, `stop`,
  `status`, `list`, `approve`, `reject`, `autostart`.
- ~~`VETTERD_NOTIFIER=linux`~~ — was not a valid value, and the claim
  that the daemon "refuses to install if `$DBUS_SESSION_BUS_ADDRESS`
  is unset and exits 78" was fiction. **Landed in Phase 6b
  (2026-09-06)**: `build_from_env` accepts `mac` / `linux` / `mock` /
  `noop`, `linux` is the default on Linux, and it does now fail closed
  with exit 78 when the session bus is unreachable.
- `[package.metadata.deb]` in `vetterd/Cargo.toml` — absent.
- `/usr/lib/systemd/user/vetter.service` — no unit file in the repo.
- `/etc/xdg/autostart/vetter.desktop` — still no *system-wide* entry,
  and there will not be one: Phase 6e writes the user-scoped
  `~/.config/autostart/vetter.desktop` instead, opt-in via **Start at
  login** rather than enabled at install time (§5.3).
- ~~`dev.vetter.daemon.svg` tray icon~~ — **landed in Phase 6c**:
  `share/icons/hicolor/scalable/apps/dev.vetter.daemon.svg`, installed
  into `$XDG_DATA_HOME` by `tools/install-desktop.sh`. The tray also
  publishes an `IconPixmap` fallback so it works uninstalled.
- `tools/release-deb.sh` — referenced by `Release.md:15`; `tools/`
  contains only the macOS scripts, `install-deps.sh`,
  `install-desktop.sh`, `dev-window.sh` and `park-spread.sh`. Under
  §4.3 this one is not coming.
- ~~`vetterd/src/runloop/linux/`~~ — **landed in Phase 6d**, though
  reached by a `#[path]` mapping in `lib.rs` rather than by nesting
  under `runloop/mod.rs`, which is `#![cfg(target_os = "macos")]` at
  file scope.
- `~/.config/vetter/icon.css` — a `gtk::CssProvider` override path
  for an icon that doesn't exist. **Deliberately still absent**:
  Phase 6h declined to add a user-override stylesheet, because a
  surface whose job is making risk legible should not invite
  restyling. GTK's own `~/.config/gtk-4.0/gtk.css` outranks our
  provider for anyone determined; that is GTK's behaviour, not a
  vetter feature.

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

### Phase 6a — Headless resolve path `[x]` **done 2026-09-06**

This is the unblocker, and it is platform-independent: **macOS is
missing it too.** It costs no new dependencies and makes vetter
immediately usable on Linux from a terminal, well before any GUI
lands. It also gives every later phase a resolve path to test
against.

- [x] Add `MgmtRequest::Resolve { id: String, decision: …, reason: Option<String> }`
      to [vetter-core/src/wire/mod.rs](../vetter-core/src/wire/mod.rs),
      plus the matching `MgmtResponse`. Reuse the existing
      allow/deny decision type rather than inventing a third.
- [x] Handle it in the daemon's admin loop
      ([vetterd/src/lib.rs](../vetterd/src/lib.rs)) by calling
      `PendingQueue::resolve`, exactly as the macOS UI callbacks do.
      Audit reason: `"approved via admin socket"` /
      `"rejected via admin socket"` — the strings the docs already
      promise.
- [x] `vet daemon approve <id>` / `vet daemon reject [--reason R] <id>`
      in [vet/src/daemon.rs](../vet/src/daemon.rs). Accept a unique
      ULID prefix, not just the full 26 chars — the ids are long and
      this is a hand-typed command.
- [x] Decide and document whether an unknown / already-resolved id
      is an error or a no-op (recommend: error, exit non-zero).
- [x] Integration coverage in
      [vetterd/tests/admin_ipc.rs](../vetterd/tests/admin_ipc.rs):
      approve unblocks the parked client with exit 0; reject
      unblocks with 77; both write the right audit reason;
      double-resolve is rejected.
- [x] Confirm this is reachable over SSH with no `$DISPLAY` /
      `$DBUS_SESSION_BUS_ADDRESS` — that is the documented headless
      story and it should finally be true.

### Phase 6b — `LinuxNotifier` (D-Bus notifications) `[x]` **done 2026-09-06**

- [x] Add `zbus` (5.19) under
      `[target.'cfg(target_os = "linux")'.dependencies]` in
      [vetterd/Cargo.toml](../vetterd/Cargo.toml), mirroring how the
      `objc2` stack is gated for macOS.
- [x] `vetterd/src/notifier/linux.rs` implementing `Notifier`:
      `Notify` with `actions = ["approve", "Approve", "reject", "Reject"]`
      (**this spec was short** — macOS registers four; see §6i),
      `desktop-entry` hint, `urgency = critical`, `expire_timeout = 0`
      (never auto-expire — a prompt must not silently vanish).
- [x] Subscribe to `ActionInvoked`, `NotificationClosed`, and
      `ActivationToken`; map notification id → request ULID;
      route into `PendingQueue::resolve`.
- [x] `CloseNotification` on every resolve path — including
      resolves that came from the admin socket or a rule addition
      (the macOS `persist_rule_async` analogue), so banners don't
      linger.
- [x] Consume `NotifyHint::was_empty_before` for coalescing (spec
      §7: "we don't spam banners"). The hint is already computed.
- [x] Cache `GetCapabilities`; re-query on `NameOwnerChanged` for
      `org.freedesktop.Notifications` (§5.4). Degrade to
      click-through when `actions` is absent.
- [x] Honour `settings.notification_sound` via the `sound-name` hint.
- [x] Add `"linux"` to `notifier::build_from_env` and make it the
      default `default_kind()` on `target_os = "linux"`. Guard on a
      reachable session bus, **not** on any bundle notion (§5.2);
      exit 78 with a message pointing at `VETTERD_NOTIFIER=noop`.
- [x] Threading: run the `zbus` connection on its own thread with a
      single-threaded async runtime. Never block the UI thread on a
      bus call.

At the end of 6b, Linux has a working approve/reject loop for the
common case, without any GUI toolkit dependency at all. **This is a
credible v0.2 ship point.**

### Phase 6c — Tray icon (StatusNotifierItem) `[x]` **done 2026-09-06**

- [x] Evaluate `ksni` (0.3.6) vs. hand-rolling the SNI object on
      the `zbus` connection we already have in 6b. Hand-rolling
      avoids a second D-Bus stack and a possible zbus-version
      conflict — check `ksni`'s zbus dependency before committing.
- [x] Publish `StatusNotifierItem` with the shield icon; register
      with `org.kde.StatusNotifierWatcher`, falling back to
      `org.freedesktop.StatusNotifierWatcher`.
- [x] Pending-count badge. Note the SNI analogue of the macOS badge
      is either `IconName`/`IconPixmap` swapping, an `OverlayIcon`,
      or `ToolTip` text — Plasma renders overlay icons, GNOME's
      extension may not. Emit `NewIcon` / `NewToolTip` after every
      change; setting properties alone does not push state.
- [x] Wire the queue's change-listener to update the badge (macOS's
      `set_pending_count` analogue).
- [x] `com.canonical.dbusmenu` context menu: **Open Vetter…**,
      **Pending: N**, **Quit Vetter** — plus per-request
      Approve/Reject items, which is genuinely useful on Wayland
      because the host positions the menu correctly (§5.1b).
- [x] Ship the icon: generate a symbolic/scalable SVG from
      `assets/vetter-logo.svg` into
      `share/icons/hicolor/scalable/apps/dev.vetter.daemon.svg`,
      and a `dev.vetter.daemon.desktop` entry (needed for the
      `desktop-entry` notification hint anyway, §5.2).
- [x] Daemon must start and stay useful when no watcher is present
      (§5.5).

### Phase 6d — Approval window (GTK4) `[x]` **done 2026-09-08**

The big one. Roughly the Linux counterpart of ~3700 lines of AppKit.

- [x] Add `gtk4` (0.11) gated to `target_os = "linux"`. Build deps
      are declared once in
      [tools/install-deps.sh](../tools/install-deps.sh) — `--check`
      to verify, `--run` to install. Don't restate package names
      here; that is how the list drifted last time.
- [x] Introduce `PlatformDriver::Gtk` alongside `None` / `AppKit`
      in [vetterd/src/lib.rs](../vetterd/src/lib.rs); `run_with_glib`
      drives the main loop on the main thread with the accept loop
      on a background thread, mirroring `run_with_appkit`.
- [x] `vetterd/src/runloop/linux/` — window + card assembly. Follow
      the macOS split: keep the pure lowering functions
      (queue → card data, effects → rows, signals → pills, URL
      segmentation) **shared and unit-tested**, and let the
      GTK module own only widget assembly. Several of the existing
      macOS helpers (`popover_url.rs`, `popover_pills.rs`,
      `popover_effects.rs`, `popover_attr.rs`) are currently
      `#![cfg(target_os = "macos")]` but are substantially
      platform-independent logic — **lifting the pure half out is a
      prerequisite, not an afterthought.**
- [x] Cards: §8.5 detail disclosure, raw-command disclosure + copy,
      signal pills, host-trust palette, `Open file` via `xdg-open`.
      *(The structured effect rows render inline on pending cards,
      matching macOS — a user deciding now should not have to open a
      disclosure. The "Show raw" disclosure carries the §8.5 body and
      the copy button. A "▸ Details" disclosure over the structured
      rows is resolved-card behaviour on macOS and lands with the
      Recent section.)*
- [x] Per-card **Approve** / **Reject**.
- [x] `Allowlist…` and `Trust host…` pickers including the duration
      radio group (15m / 1h / 4h / this terminal session / Forever).
      These drive `AddRule` / `AddKnownHost`, which already work.
      *(Modal `gtk::Window` sheets, not `Dialog`: that type is
      deprecated in GTK 4.10 and its replacement needs the gtk4
      crate's `v4_10` feature, which would raise our floor above
      Debian stable. The rule preview is mandatory and rendered with
      `set_text`.)*
- [x] `See approval reason` + **Revoke rule** on auto-allow Recent
      cards, driving the existing `RemoveRule`. Needed the Recent
      section, which landed alongside it.
- [x] Footer: **Quit Vetter**, **Play sound on new request**.
      **Start at login** shipped insensitive here and went live in
      Phase 6e, which replaced the non-macOS stub with a real XDG
      entry (§5.3).
- [x] Notification click-through opens the window scrolled to the
      matching card, using the activation token (§5.6). The body
      click is registered as the spec's `default` action and never
      resolves — it opens the window and leaves the request pending.
- [x] Single-instance / raise-existing entry point (§5.5), as
      `MgmtRequest::OpenWindow` + `vet daemon open`. GNOME ships no
      StatusNotifierHost, so the tray cannot be the only way in.
      *(Single-instance is a `NameHasOwner` check made before the
      `Application` is built, not `ApplicationFlags::IS_SERVICE` —
      that flag was tried and removed, because its inactivity
      timeout ended the main loop and shut the whole daemon down
      after ~10 s idle.)*
- [x] Threading discipline: GObject is single-threaded. The queue's
      change-listener fires on whatever worker thread resolved the
      request and must hop to the GTK thread via
      `glib::idle_add_local` / `MainContext::invoke` before touching
      widgets. This is the macOS `MainThreadBound` pattern.

### Phase 6e — Autostart + `vet doctor` parity `[x]` **done 2026-09-08**

- [x] Linux `autostart::sys` writing/removing
      `~/.config/autostart/vetter.desktop` (§5.3); `current()` is a
      file-existence + `Hidden=` check.
      Also honours `X-GNOME-Autostart-enabled=false`, which is what
      GNOME Tweaks writes — without that, `reconcile_with_settings`
      would silently re-enable autostart a user had just switched off.
      The entry pins an absolute `Exec=`, so `current()` stats that
      target and reports `NotFound` when the binary has moved: the one
      way this is worse than `SMAppService`, and detecting it is the
      mitigation.
- [x] `reconcile_with_settings` then works unchanged on Linux — it
      already no-ops only on `Unsupported`. Verified, not assumed: it
      started converging real state the moment the backend landed,
      with no edit of its own.
- [x] `vet doctor` autostart row: drop the `cfg!(target_os = "macos")`
      early return, report the real state.
      The per-status wording is `cfg`-split so macOS output stays
      byte-identical. `vet daemon autostart`'s own CLI wording had the
      same leak — it told Linux users their "login item" was
      registered — and is split the same way.
- [x] `vet doctor` "code signing" row: on Linux report package
      provenance instead (`rpm -V` / dpkg verify when installed from
      a package; `INFO built from source` otherwise), or keep the
      SKIP with an honest reason.
      **Chose provenance without integrity verification.** The row
      asks `rpm -qf` / `dpkg-query -S` who owns the binary and reports
      that; it deliberately does *not* run `rpm -V`, which checksums
      every file in the owning package and would cost seconds on a
      command people run when something is already wrong — to prove a
      guarantee we cannot currently make, since no signed Vetter
      package exists until 6f. Today's honest answer for essentially
      every install is `INFO built from source`.
- [x] New Linux-only doctor rows worth having, given §5.4/§5.5:
      session bus reachable; notification daemon present +
      `actions` capability; StatusNotifierWatcher present;
      `$XDG_RUNTIME_DIR` sane.
      Plus a `desktop entry` row: absent, it is invisible — banners
      and the window silently fall back to a generic name and icon
      (§5.2) — so it earns a line. The tray row distinguishes "no
      watcher" from "watcher but no host", because the second looks
      exactly like a Vetter bug from the outside: our item registers
      and then nobody draws it.
      The two bus-dependent rows are answered by the **daemon** over
      the admin socket (`MgmtRequest::GetDesktopHealth`), not probed
      by `vet`. `vet` is exec'd for every wrapped command, and linking
      a D-Bus stack into it to serve one diagnostic would tax the hot
      path to pay for `vet doctor`. Cost: those rows read "not probed"
      when the daemon is down — which the `daemon` row above already
      explains.
      Framing throughout: most missing states here are supported
      steady states, not faults. `WARN` is reserved for losing a
      capability you would otherwise have; a session with no tray host
      is `INFO`, because notifications and `vet daemon approve` are
      unaffected.

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

### Phase 6g — CI `[~]` **three of four done**

- [x] Linux job already runs fmt/clippy/tests. Extend it to build
      the new `target_os = "linux"` cfg paths — otherwise the
      notifier and GTK code never get compiled in CI. *(Falls out of
      the deps being installed: the clippy, test and msrv jobs all
      run on `ubuntu-latest`, so `--all-targets` compiles the zbus /
      ksni / gtk4 code as a matter of course.)*
- [x] Run `tools/install-deps.sh --run` in the Linux job instead of
      naming packages in the workflow. *(In the clippy, test and
      msrv jobs.)*
- [x] Keep the `MockNotifier` as the automated-coverage workhorse;
      CI has no session bus, so 6b/6c/6d get manual smoke coverage
      only (§7) plus unit tests on the extracted pure functions.
      *(Held: the pure halves live in `vetterd/src/cards/` and are
      tested on every platform.)*
- [ ] Consider a `dbus-run-session` + `dunst` job to smoke the
      `LinuxNotifier` for real. `dunst` supports `actions`, so an
      end-to-end approve could genuinely be automated. Worth
      prototyping — this would be better coverage than macOS has.
      *(Unbuilt, but no longer blocked: this configuration — a
      private bus with no notification server — used to stall the
      daemon for ~60 s at startup, fixed 2026-09-09. `dunst` is not
      installed on the reference host.)*

### Phase 6i — Notification and tray parity `[x]` **done 2026-09-08**

Found on 2026-09-08 by using the finished surface: if you miss the
banner, the tray can only *decide* a request, never open it — and the
banner itself offers two of the four actions macOS does.

Both gaps trace to this document under-describing macOS. §3.2 said
"Notification with Approve/Reject" and §6b's checklist specified
exactly two action keys, so 6b met its spec and the shortfall was
invisible. Recorded here rather than folded into 6h: this is missing
function, not rough styling.

Both are cheap now that 6d step 4 built the open-window-scrolled-to-a-
card plumbing and step 3 built the pickers.

- [x] Add `Allowlist…` and `Trust host…` actions to the Linux
      notification. A freedesktop notification cannot host a picker,
      so — exactly as macOS does — the action opens the window with
      that card's picker raised and leaves the request pending.
- [x] Offer `Trust host…` only when the host is unknown, mirroring
      macOS's second notification category.
      `cards::picker::show_trust_host` already answers this.
- [x] Check how Plasma renders four action buttons before committing
      to four. macOS condenses the overflow under an "Options"
      dropdown; the freedesktop spec has no equivalent, so this may
      need to degrade to two buttons plus the body click.
      **Answered by screenshot on Plasma 6.7.3:** all four render
      inline, untruncated — the banner simply widens — and the
      reserved `default` key correctly draws no button. Four kept.
      Caveat: measured on a 4480px-wide display; a narrow screen may
      still wrap, and the array is ordered so a truncating server
      drops the shortcuts rather than Approve/Reject.
- [x] Add **Open** to the tray's per-request submenu alongside
      Approve / Reject, so a request can be inspected from the tray
      rather than only decided blind. `request_show(Some(id))` from
      6d step 4 is the call.

### Phase 6h — Visual polish `[x]` **done 2026-09-09**

Done. The reference is [ApprovalUI.md](ApprovalUI.md); the work was
checked against screenshots in both themes rather than reasoned about,
using a nested `Xephyr` display so the window could be captured
deterministically instead of fighting the compositor for stacking
order.

#### Divergence audit vs. `ApprovalUI.md`

| Element | Status | Divergence |
|---|---|---|
| Tinted pill recipe | matches | `alpha(@color, 0.22)` in CSS is the literal translation of `pill_bg_for` |
| Popover appearance | **deliberate** | macOS *pins* Dark Aqua because its tones were calibrated for a dark ground. A GTK window ignoring the user's theme would look broken beside every other app, so we track the theme and carry two markup palettes instead |
| Signal pills | matches | dedupe by kind, priority sort, `slug: detail` tooltip |
| URL row | matches — **was** accidental | had been proportional; now monospace, as the spec always said |
| Effect rows | **deliberate** | no SF-Symbol glyphs: no equivalent icon set, and inventing one per row would read worse than the text label |
| Show raw disclosure | matches | `GtkExpander`, collapsed by default |
| Body colouring | matches | the SGR mapping is the shared `cards::spans` model |
| Dry-run wrapper | matches — **was** accidental | titled orange frame; the inner card now drops its own border so the wrapper is the only frame, not a second one 3px inside the first |
| Card chrome | **deliberate** | no separator rule between cards. The spec's rule divides cards in a flat AppKit stack; ours are drawn boxes, and a rule on top of a border reads as a stray line |
| Approve / Reject | **theme-dependent** | `suggested-action` / `destructive-action` render filled under Adwaita and outlined under Breeze. Left to the theme rather than hard-coded, which is the GTK convention |
| Footer | matches | Quit plus the two settings checkboxes |

The largest accidental divergence: `.card` is a **libadwaita** style
class and this binary links plain GTK4, so under Breeze — the theme a
KDE session hands us — it resolved to nothing and cards rendered as
flat text separated by a rule. The window default had then been sized
against those surface-less cards, so once real padding existed it
showed one-and-a-bit cards with the next one cut off at the footer.

- [x] Audit each element against `ApprovalUI.md` (table above).
- [x] Spacing, padding, corner radii, card separation. Cards are drawn
      surfaces (`.vetter-card`: `@theme_base_color`, `@borders`, 8px
      radius, 12px inset) with `CARD_SPACING` between and a tighter
      `ROW_SPACING` within, so the grouping reads.
- [x] Typography: URL row and card title monospaced, title semibold,
      matching the macOS header font. The URL is the field a user
      scans for a look-alike host, and proportional glyphs are exactly
      where homoglyphs hide.
- [x] Palette fidelity in light **and** dark, verified by screenshot
      in each. Every CSS colour is a GTK *named* colour so the sheet
      re-resolves per theme; only the raw body's Pango markup needs
      the two hard-coded palettes.
- [x] Live theme switching, via
      `connect_gtk_application_prefer_dark_theme_notify` →
      `request_refresh`.
- [x] Window sizing: 720x720 default, 480x360 floor, both pinned by a
      `const` assertion so a bad edit fails the build rather than a
      test. Empty state is a centred title plus a line naming the
      other approval routes.
- [x] **No user CSS override**, deliberately. This surface exists to
      make risk legible, and a stylesheet we invited users to edit is
      a way to make a hostile request look benign on the very screen
      where it is authorised. §3.3 also lists `~/.config/vetter/icon.css`
      as a path the old docs promised and never had. GTK already loads
      `~/.config/gtk-4.0/gtk.css` at `PRIORITY_USER`, which outranks
      ours, for anyone determined — that is GTK's decision, not a
      vetter feature, and is not advertised as one.

Not chased, with reasons: SF-Symbol-equivalent row glyphs (no
comparable icon set; text labels beat invented icons); per-param query
colouring and click-to-pin pill popovers — both are listed as *future
work* for macOS, so building them here would put Linux ahead of the
reference rather than level with it.

---

## 7. Manual smoke test (Linux) — **all 15 steps runnable**

Every step is runnable as of 2026-09-09: 6a–6e plus 6h and 6i have
landed. Each step keeps the phase tag that introduced it, as
provenance rather than as a gate. Step 14's logout/login half still
needs a human — nothing scripted can verify that the daemon returns
after a real session restart.

1. Build: `cargo build --release -p vetterd -p vet`, then
   `export PATH="$PWD/target/release:$PATH"`. *(works today)*
2. `vet daemon start`; `vet doctor` should be all OK/INFO.
   *(works today)*
3. `vet curl https://prompt-test.example/` parks; `vet daemon list`
   shows it with a ULID. *(works today)*
4. **Headless resolve.** `vet daemon approve <id>` → the parked
   `vet` exec's curl and returns curl's exit code.
   `vet daemon reject <id>` → exit 77. Works over SSH with no
   `$DISPLAY`. A unique ULID prefix is accepted in place of the full
   26 characters. *(6a — works today)*
5. **Notification path.** A Plasma notification appears with
   **Approve** and **Reject**. Clicking either resolves the request
   and the banner clears. *(6b — works today)*
6. **Coalescing.** Two concurrent `vet curl` commands raise only one
   banner; the second coalesces. *(6b — works today)*
7. **Capability degradation.** Repeat under a daemon without
   `actions` (e.g. `notify-osd`); confirm the daemon still posts the
   banner and warns that buttons are unavailable. (The body-click
   path into the window arrives with 6d; until then the fallback is
   `vet daemon approve`.) *(6b — works today)*
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
- **Allowlist affordance in the tray menu (deferred 2026-09-06).**
  §5.1b writes DBusMenu off as good enough only for Approve /
  Reject / Open Vetter… / Quit, but it does support submenus and
  checkmarks — enough to express the duration radio group
  (15m / 1h / 4h / session / Forever) as
  `Allowlist this host ▸`. That would deliver GUI allowlisting in
  6c rather than waiting for 6d, which matters if 6d slips. The
  trade is real, not free: a menu cannot show the *rule preview*
  the macOS picker puts in front of the user before persisting a
  rule, so it trades reviewability for reach on a
  security-relevant write. Considered and deliberately deferred —
  6c stays as specced. Revisit if 6d slips.
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
| Build deps | `tools/install-deps.sh --run` (resolves dnf names) | same script (resolves apt names) |
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
  hatch — *was* unimplemented on **every** platform, and landed in
  Phase 6a (2026-09-06). `vet daemon open` joined them in 6d step 4.
- **`Release.md` §"Linux / Launchpad PPA"** documents a Launchpad
  flow and a `tools/release-deb.sh` that does not exist. Under §4.3
  this section is on hold; it should be marked as such rather than
  read as current.
- **`TestingPlan.md`:690** points at the old `UbuntuApp.md` smoke
  procedure. Repoint at §7 here. (The "gated on unlanded phases"
  caveat this bullet used to carry is spent — as of 2026-09-09 all
  15 steps run.)
- **`ApprovalUI.md`** should gain a note that the Linux container is
  a window, not a popover (§5.1). **Still outstanding** — checked
  2026-09-09, the file has no such note, and it is editable
  (unlike `Overview.md` / `TestingPlan.md`).
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
