# Release — signing, notarisation, distribution

Operational guide for cutting a distribution-quality artefact on each
supported platform: the Developer-ID signed + notarised + stapled
`Vetter.app` shipped through the Homebrew cask (macOS), and the
`.deb` shipped through the Launchpad PPA (Ubuntu / Linux).

The everyday developer build paths (ad-hoc signed `.app` on macOS;
`cargo build` + `cargo deb` on Ubuntu) live in
[MacOSApp.md](MacOSApp.md) and [UbuntuApp.md](UbuntuApp.md)
respectively; use those for local hacking, this for shipping.

Both pipelines are intentionally **local-only** for now —
[`tools/release.sh`](../tools/release.sh) (macOS) and
[`tools/release-deb.sh`](../tools/release-deb.sh) (Ubuntu) run on a
maintainer machine with the credentials below in env. Wiring the
same pipelines into a GitHub Actions release workflow on tag push is
a tracked follow-up (`TODO.md` *Backlog → Deferred from Phase 4 /
Phase 5*).

---

# macOS / Homebrew tap

## Apple-side prerequisites (one-time)

`tools/release.sh` refuses to run without all four. Acquire them
once; stash the secrets outside the repo (`~/.private/`, mode 600)
and source them from a per-shell rc file when you cut a release.

### 1. Apple Developer Program membership

Sign up at <https://developer.apple.com/programs/> ($99/yr,
individual is fine). Notarisation is gated on a paid membership;
Developer ID certificates are not issued to free accounts.

### 2. Developer ID Application certificate

In Keychain Access → Certificate Assistant → Request a Certificate
from a Certificate Authority… → save to disk (this is your CSR).
Then at <https://developer.apple.com/account/resources/certificates>
choose **Developer ID Application**, upload the CSR, download the
issued `.cer`, and double-click it to install into your login
keychain.

Confirm with:

```sh
security find-identity -v -p codesigning
```

You should see exactly one line like:

```
1) ABCDEF1234567890... "Developer ID Application: Your Name (TEAMIDXX)"
```

The full quoted string (including `Developer ID Application: ` and
the trailing `(TEAMIDXX)`) is what you'll pass as
`$DEVELOPER_ID_APPLICATION`.

### 3. Notary API key (App Store Connect)

`xcrun notarytool` supports two auth modes; the API-key path is the
right one for unattended scripts. At
<https://appstoreconnect.apple.com/access/integrations/api> →
**Generate API Key** with the **Developer** role:

- Download the `.p8` file **immediately** — App Store Connect will
  not let you re-download it. Lose it and you generate a new one.
- Note the 10-character **Key ID** (visible in the table) and the
  **Issuer** UUID (top of the page).

Stash the `.p8` outside the repo:

```sh
mkdir -p ~/.private && chmod 700 ~/.private
mv ~/Downloads/AuthKey_XXXXXXXXXX.p8 ~/.private/
chmod 600 ~/.private/AuthKey_XXXXXXXXXX.p8
```

### 4. Team ID

Visible at <https://developer.apple.com/account> under **Membership
Details**. Same 10-character string that appears in parens at the
end of your Developer ID identity (`Developer ID Application: Your
Name (TEAMIDXX)`).

### Putting it together

A `~/.private/vetter-release.env` like this is the recommended
setup; `source` it before each release:

```sh
export DEVELOPER_ID_APPLICATION="Developer ID Application: Your Name (TEAMIDXX)"
export TEAM_ID="TEAMIDXX"
export AC_API_KEY_PATH="$HOME/.private/AuthKey_XXXXXXXXXX.p8"
export AC_API_KEY_ID="XXXXXXXXXX"
export AC_API_KEY_ISSUER="00000000-0000-0000-0000-000000000000"
```

`chmod 600 ~/.private/vetter-release.env` and never check it in.

## Cross-arch toolchains

Distribution bundles are universal Mach-Os (`lipo`-ed x86_64 +
arm64), so cargo needs both rustup targets:

```sh
rustup target add x86_64-apple-darwin aarch64-apple-darwin
```

Homebrew rust ships only the host arch and will fail at the cargo
build step. If `which cargo` points at `/opt/homebrew/bin/cargo`
you're on Homebrew rust; install rustup
(<https://rustup.rs>) and re-run.

## Cutting a release

```sh
# 1. Confirm `Cargo.toml` workspace.package.version is what you
#    want to ship; bump if not.
git diff Cargo.toml

# 2. Tag the commit you intend to release.
VERSION=$(grep '^version' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
git tag "v$VERSION"

# 3. Source the credentials and run the release script.
source ~/.private/vetter-release.env
tools/release.sh

# 4. Push the tag.
git push origin "v$VERSION"
```

The script's final stdout looks like:

```
release.sh: signed + notarised + stapled bundle ready.
  app:    target/Vetter.app
  zip:    target/Vetter-0.1.0.zip
  sha256: <64-hex chars>

Paste into the tap repo's Casks/vetter.rb (then commit + push):
  version "0.1.0"
  sha256 "<64-hex chars>"
```

## Publishing the Homebrew cask

The cask lives in its own repo,
[`blevinstein/homebrew-vetter`](https://github.com/blevinstein/homebrew-vetter)
— Homebrew taps must be standalone repos named
`homebrew-<name>`, and keeping it separate means tap users can
`brew tap` without cloning the whole vetter source tree. Clone it
once:

```sh
git clone git@github.com:blevinstein/homebrew-vetter ~/dev/homebrew-vetter
```

Then for each release, run [`tools/publish-cask.sh`](../tools/publish-cask.sh)
from the `vetter` repo root:

```sh
tools/publish-cask.sh
```

The script (1) creates the GitHub Release on `blevinstein/vetter`
tagged `v$VERSION` with `target/Vetter-$VERSION.zip` attached so
the cask `url` resolves, (2) bumps the `version` + `sha256`
stanzas in the tap's `Casks/vetter.rb` in place, and (3) commits
and pushes the cask change to `main`. It is idempotent — re-runs
after a botched publish re-upload the asset (`--clobber`) and
skip the cask commit when nothing changed.

Override the defaults via env vars when needed:

```sh
TAP_DIR=/some/other/clone RELEASE_REPO=fork-owner/vetter tools/publish-cask.sh
```

Until the CI release workflow lands this is a manual invocation;
the workflow will eventually `tools/release.sh && tools/publish-cask.sh`
from a job triggered on tag push (with `GITHUB_TOKEN` instead of
`gh auth login`, and a deploy-key https remote on the tap clone
instead of ssh), and will likely open a PR against
`blevinstein/homebrew-vetter` rather than pushing directly to
`main` so the tap stays reviewable.

End users then install with:

```sh
brew tap blevinstein/vetter
brew install --cask vetter
open /Applications/Vetter.app                  # first launch grants notification permission
```

`vet` lands in `$(brew --prefix)/bin` via the cask's `binary`
stanza, so `vet curl …` works immediately. Existing users on a
prior version pick up the bump with
`brew update && brew upgrade --cask vetter`.

## Verification

After install, on a fresh machine (or a machine where you first
remove the quarantine xattr Apple adds to downloaded apps):

```sh
# Gatekeeper acceptance — should print "accepted" (Notarized Developer ID).
spctl --assess --type execute --verbose=4 /Applications/Vetter.app

# Codesign metadata — should show the Developer ID identity, the
# Team ID, and `flags=0x10000(runtime)` (hardened runtime active).
codesign -dvv /Applications/Vetter.app/Contents/MacOS/vetterd
codesign -dvv /Applications/Vetter.app/Contents/MacOS/vet

# Stapler — should print "The validate action worked!".
xcrun stapler validate /Applications/Vetter.app
```

If `spctl` rejects with "rejected (the code is valid but does not
seem to be an app)", the bundle wasn't signed end-to-end (see
troubleshooting). If `codesign -dvv` shows `flags=0x0`, the
`--options runtime` flag was missing and notary will have rejected
already.

## Troubleshooting

- **`notarytool submit --wait` exits with `status: Invalid`.** The
  notarisation log explains why; fetch it with the submission UUID
  the script printed:

  ```sh
  xcrun notarytool log <submission-uuid> \
      --key "$AC_API_KEY_PATH" --key-id "$AC_API_KEY_ID" \
      --issuer "$AC_API_KEY_ISSUER"
  ```

  Common causes (in rough order of frequency):
  - The Developer ID cert is the wrong type (must be **Developer ID
    Application**, not "Mac App Distribution" or "Developer ID
    Installer").
  - One of the inner Mach-Os was not signed with `--options
    runtime`. The release script signs every binary in
    `Contents/MacOS/` explicitly; if you've added a new helper,
    extend the loop.
  - The bundle uses `--deep` signing. Apple stopped accepting that
    for notarisation; the release script signs each Mach-O before
    signing the bundle.
  - The zip was made with `zip` instead of `ditto` and stripped
    metadata codesign / notary depend on.

- **`stapler staple` fails with "The staple and validate action
  failed!".** Notarisation hasn't propagated yet; wait 30 seconds
  and re-run. If it persists, the submitted artifact isn't the same
  bundle you're trying to staple — confirm `tools/release.sh`
  hasn't been re-run between the submit and the staple steps.

- **End-user sees "Vetter is damaged and can't be opened" on first
  launch.** The bundle made it past notary but the staple is
  missing; the user is offline so Gatekeeper can't fetch the
  ticket. Either re-run `tools/release.sh` (which always staples)
  or re-staple by hand (`xcrun stapler staple Vetter.app`) and
  re-zip with `ditto`.

- **`security find-identity` lists the Developer ID identity but
  the script can't see it.** Either the keychain is locked
  (`security unlock-keychain ~/Library/Keychains/login.keychain-db`)
  or you have multiple identities and the substring match in
  `tools/release.sh` is hitting the wrong one — be more specific in
  `$DEVELOPER_ID_APPLICATION`, including the parenthesised Team ID.

- **`brew install --cask vetter` fails with HTTP 404 on the
  download URL, or `brew info` keeps showing the previous
  version after a release.** Two failure modes look the same
  from the outside; check both:
  1. The cask was bumped + pushed but the GitHub Release was
     never created (or the `.zip` asset wasn't attached). The
     cask `url` resolves to a real Release page, not just a tag,
     so a bare tag without a Release returns 404. Confirm with
     `gh release view "v$VERSION" --repo blevinstein/vetter`; if
     it errors, run step 1 of the publish block above.
  2. The user's local tap clone at
     `$(brew --repository blevinstein/vetter)` hasn't fetched
     the new cask commit yet. `brew update` (or
     `brew tap --repair blevinstein/vetter`) refreshes it. CI
     can't help here — it's a per-user cache miss — so the
     end-user-install instructions always recommend
     `brew update && brew upgrade --cask vetter` for upgrades.

- **`vet daemon start` works locally but fails after `brew install
  --cask vetter`.** The cask installs `Vetter.app` to
  `/Applications/`; the daemon's bundle-location check (the
  `VETTERD_NOTIFIER=mac` guard from `plans/MacOSApp.md`) accepts
  any `.app` under `/Applications/Vetter.app/Contents/MacOS/`, so
  there's nothing cask-specific to fix here. The user does need to
  `open /Applications/Vetter.app` once on first install so macOS
  can run the notification-permission dialog and (optionally) the
  user can opt into autostart by ticking the popover's **Start at
  login** checkbox; thereafter the bundle relaunches itself at
  every login via `SMAppService.mainApp` — see
  [plans/MacOSApp.md § Autostart on login](MacOSApp.md#autostart-on-login).

## What's not here yet (macOS)

Tracked in [`TODO.md`](../TODO.md) *Backlog → Deferred from Phase 4 /
Phase 5*:

- **CI release workflow.** A GitHub Actions job that imports the
  Developer ID certificate from a base64 secret, the `.p8` from
  another secret, then runs `tools/release.sh` followed by
  `tools/publish-cask.sh` (ideally pointing the latter at a PR
  branch on `blevinstein/homebrew-vetter` rather than direct-to-
  `main`). Both scripts are structured so the workflow can call
  them verbatim — only the auth sourcing differs (`gh auth login`
  → `GITHUB_TOKEN`; ssh tap remote → deploy-key https remote).
- **DMG packaging.** Cask handles either `app "..."` (zip-style,
  what we ship) or `app "..." within ".dmg"` (DMG with custom
  background). Zip is enough for v0.1.
- **Sparkle / in-app updates.** Homebrew is the update channel for
  now; `brew upgrade --cask vetter` is the supported path.

---

# Linux / Launchpad PPA

Operational guide for cutting a distribution-quality `.deb` and
publishing it to the project's Launchpad PPA so end users get
`sudo apt-get install vetter` semantics. Source-build instructions
for local development live in [UbuntuApp.md](UbuntuApp.md).

## Launchpad-side prerequisites (one-time)

`tools/release-deb.sh` refuses to run without all four. Acquire
them once; the GPG key is the only secret material that lives on
disk, and it should be passphrase-protected.

### 1. Launchpad account

Sign up at <https://login.launchpad.net/>. Free; required for PPA
hosting.

### 2. PPA created on Launchpad

At <https://launchpad.net/~blevinstein/+activate-ppa> create
`ppa:blevinstein/vetter`. The form sets the PPA name, description,
and the dependency series (jammy = 22.04, noble = 24.04 — enable
both). Launchpad allocates a build farm slot for amd64 and arm64
automatically.

### 3. GPG signing key

Source uploads to a PPA must be signed by a GPG key registered on
the uploader's Launchpad profile.

```sh
gpg --full-generate-key                 # RSA, 4096 bits, no expiry
gpg --list-secret-keys --keyid-format LONG
gpg --send-keys <KEYID>                 # publishes to the GPG keyserver pool
```

At <https://launchpad.net/~blevinstein/+editpgpkeys> paste the key
fingerprint and respond to the encrypted confirmation email. The
profile shows the key as "verified" within ~5 minutes.

### 4. `dput` configured

`dput` ships with Ubuntu (`sudo apt-get install dput`). One-time
config in `~/.dput.cf`:

```ini
[vetter-ppa]
fqdn = ppa.launchpad.net
method = ftp
incoming = ~blevinstein/ubuntu/vetter/
login = anonymous
allow_unsigned_uploads = 0
```

### Putting it together

A `~/.private/vetter-release-deb.env` like this is the recommended
setup; `source` it before each release:

```sh
export GPG_SIGN_KEY="ABCDEF1234567890ABCDEF1234567890ABCDEF12"
export DEBFULLNAME="Your Name"
export DEBEMAIL="you@example.com"
```

`chmod 600 ~/.private/vetter-release-deb.env` and never check it in.

## Cutting a release

```sh
# 1. Confirm Cargo.toml workspace.package.version is what you
#    want to ship; bump if not.
git diff Cargo.toml

# 2. Tag the commit you intend to release.
VERSION=$(grep '^version' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
git tag "v$VERSION"

# 3. Source the credentials and run the release script.
source ~/.private/vetter-release-deb.env
tools/release-deb.sh
```

The script:

1. Runs `cargo build --release -p vetterd -p vet` (host arch only;
   Launchpad's build farm cross-compiles arm64 from the source
   package).
2. Calls `cargo deb -p vetterd --no-build` to produce the binary
   `.deb` for local smoke (`sudo dpkg -i target/debian/vetter_*.deb`).
3. Builds the source package with `debuild -S -sa -k$GPG_SIGN_KEY`,
   producing `vetter_<VERSION>_source.changes` plus the `.dsc` /
   `.tar.xz` pair under `target/source-package/`.
4. Uploads via `dput vetter-ppa target/source-package/vetter_<VERSION>_source.changes`.

Final stdout looks like:

```
release-deb.sh: source package uploaded to ppa:blevinstein/vetter.
  source: vetter_0.2.0~jammy1
  changes: target/source-package/vetter_0.2.0~jammy1_source.changes

Track the build at:
  https://launchpad.net/~blevinstein/+archive/ubuntu/vetter/+packages
```

Launchpad emails the result of each per-arch build (~10–30
minutes); a successful build flips the package to "Published" and
it becomes installable via `apt-get`.

## Publishing across releases

Each Ubuntu LTS series (jammy / noble / future) needs its own
upload because the source package's `debian/changelog` distribution
field pins it to one series. The release script loops the upload
once per `[ jammy noble ]` entry in
`tools/release-deb.sh` (the suffix `~jammy1` / `~noble1` keeps
versions monotonic per series).

## End-user verification

After the PPA finishes building, verify on a fresh Ubuntu machine:

```sh
sudo add-apt-repository ppa:blevinstein/vetter
sudo apt-get update
sudo apt-get install vetter

# Service is active under systemd --user:
systemctl --user status vetter.service          # Active: active (running)

# Daemon socket + admin socket are on the bus:
vet daemon status                               # running, pid=…, pending=0

# Notification + tray smoke (graphical session only):
vet curl https://prompt-test.example/           # banner with Approve/Reject
```

If `apt-get install` reports `Unable to locate package vetter`, the
PPA either failed to build (check the Launchpad URL above) or the
machine's release series isn't enabled in the PPA settings. If
`systemctl --user status` reports `not loaded`, the `postinst` did
not run — confirm with `sudo dpkg --configure -a` and re-check.

## Troubleshooting

- **`debuild` fails with `gpg: signing failed: No secret key`.**
  `$GPG_SIGN_KEY` is wrong or the key is not in the `gpg` keyring
  the user invoking the script can read. `gpg --list-secret-keys`
  must list it; `secret-tool` may need to unlock it.
- **`dput` upload rejects with `incoming: not allowed`.** The PPA
  was created under a different name or owner; the
  `incoming = ~blevinstein/ubuntu/vetter/` path in `~/.dput.cf`
  must match the URL Launchpad shows on the PPA page.
- **Launchpad build fails with `dependencies not satisfied`.** The
  `Build-Depends:` line in `debian/control` (generated by
  `cargo-deb`) lists a package not in the target Ubuntu series.
  Confirm with `apt-cache madison <pkg>` against the series chroot;
  the most common offenders are `libgtk-4-dev` (jammy backports
  vs. noble main) and `librust-zbus-dev` (we usually link against
  the vendored crate, not the OS package, but `cargo-deb` can be
  miscoaxed into adding it).
- **`vetter.service` reports `condition failed`.** The unit's
  `ConditionUser` and `ConditionEnvironment` guards fired; this is
  expected on machines without a graphical session. See
  [UbuntuApp.md](UbuntuApp.md) §"Headless / SSH path".

## What's not here yet (Linux)

Tracked in [`TODO.md`](../TODO.md) Phase 6:

- **CI release workflow on tag push.** Same shape as the macOS
  follow-up: a GitHub Actions job that imports the GPG private key
  from a secret, runs `tools/release-deb.sh`, attaches the source
  `.changes` pair to a GitHub Release, and uploads to the PPA via
  `dput` from CI.
- **AppImage / snap / Flatpak.** Out of scope for v0.2; the PPA
  covers Ubuntu 22.04+ which is the supported baseline. Other
  distros can `cargo install` from the source path.
- **Debian-proper upload.** The `.deb` produced here is the
  PPA flavour. Submitting to Debian unstable requires a Debian
  Maintainer sponsor and conforming `debian/` packaging — a
  separate, much longer cycle.
