# Release — Developer ID signing, notarisation, Homebrew tap

Operational guide for cutting a distribution-quality `Vetter.app`:
Developer-ID signed, notarised by Apple, stapled, packaged into a
Homebrew cask. The everyday developer build path (ad-hoc signed,
no notary round-trip) lives in [MacOSApp.md](MacOSApp.md); use that
for local hacking, this for shipping.

The pipeline is intentionally **local-only** for now —
[`tools/release.sh`](../tools/release.sh) runs on a Mac with the
credentials below in env. Wiring the same pipeline into a GitHub
Actions release workflow on tag push is a tracked follow-up
(`TODO.md` Phase 4).

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

Then for each release:

1. Edit `~/dev/homebrew-vetter/Casks/vetter.rb` to match the printed
   `version` + `sha256` lines.
2. Commit + push on `main`:
   ```sh
   cd ~/dev/homebrew-vetter
   git commit -am "vetter v$VERSION"
   git push
   ```
3. Create a GitHub Release on `blevinstein/vetter` tagged
   `v$VERSION` and attach `target/Vetter-$VERSION.zip` so the cask
   `url` (`.../releases/download/v$VERSION/Vetter-$VERSION.zip`)
   resolves.

Until the CI release workflow lands the cask edit + tap push are
manual; the workflow will eventually open a PR against
`blevinstein/homebrew-vetter` from CI using a deploy key.

End users then install with:

```sh
brew tap blevinstein/vetter
brew install --cask vetter
open /Applications/Vetter.app                  # first launch grants notification permission
```

`vet` lands in `$(brew --prefix)/bin` via the cask's `binary`
stanza, so `vet curl …` works immediately.

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

- **`vet daemon start` works locally but fails after `brew install
  --cask vetter`.** The cask installs `Vetter.app` to
  `/Applications/`; the daemon's bundle-location check (the
  `VETTERD_NOTIFIER=mac` guard from `plans/MacOSApp.md`) accepts
  any `.app` under `/Applications/Vetter.app/Contents/MacOS/`, so
  there's nothing cask-specific to fix here — but `vet daemon
  start` won't auto-launch the bundle, the user has to `open
  /Applications/Vetter.app` once. Document this in any user-facing
  install doc; the script does not work around it.

## What's not here yet

Tracked in [`TODO.md`](../TODO.md) Phase 4:

- **CI release workflow.** A GitHub Actions job that imports the
  Developer ID certificate from a base64 secret, the `.p8` from
  another secret, runs `tools/release.sh`, attaches the zip to a
  GitHub Release, and opens a PR against `blevinstein/homebrew-vetter`
  with the bumped cask. This script is structured so the workflow
  can call it verbatim — only the env-var sourcing differs.
- **DMG packaging.** Cask handles either `app "..."` (zip-style,
  what we ship) or `app "..." within ".dmg"` (DMG with custom
  background). Zip is enough for v0.1.
- **Sparkle / in-app updates.** Homebrew is the update channel for
  now; `brew upgrade --cask vetter` is the supported path.
