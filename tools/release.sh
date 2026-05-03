#!/usr/bin/env bash
# tools/release.sh — build a Developer-ID signed, notarised, stapled
# Vetter.app for distribution.
#
# Inputs (env vars; the script refuses to run with any unset):
#   DEVELOPER_ID_APPLICATION  full identity, e.g.
#                              "Developer ID Application: Your Name (TEAMID)"
#   TEAM_ID                   Apple Team ID, used for `--team-id` on notarytool
#   AC_API_KEY_PATH           path to the .p8 from App Store Connect
#   AC_API_KEY_ID             Key ID (10-char string)
#   AC_API_KEY_ISSUER         Issuer UUID
#
# Outputs (under target/):
#   Vetter.app                  the stapled bundle
#   Vetter-<VERSION>.zip        the bundle re-zipped with the staple,
#                               via ditto so the resource-fork metadata
#                               codesign + notary rely on survives
#
# Final stdout prints the lines you paste into the tap repo's
# Casks/vetter.rb (canonical location: blevinstein/homebrew-vetter).
#
# Apple-side prerequisites (Developer Program membership, Developer ID
# certificate, Notary API key, Team ID) are documented in
# plans/Release.md. CI release-on-tag is a follow-up; this script is the
# canonical local pipeline.

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "release.sh: this script only makes sense on macOS" >&2
    exit 64
fi

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

# 1. Prerequisite check — runs before any cargo build, so missing creds
#    fail in <1s instead of after a multi-minute cross-arch build.
missing=()
for v in DEVELOPER_ID_APPLICATION TEAM_ID AC_API_KEY_PATH AC_API_KEY_ID AC_API_KEY_ISSUER; do
    if [[ -z "${!v:-}" ]]; then
        missing+=("$v")
    fi
done
if (( ${#missing[@]} > 0 )); then
    {
        echo "release.sh: missing required environment variables:"
        printf '  - %s\n' "${missing[@]}"
        echo
        echo "See plans/Release.md for how to acquire each (Apple Developer Program"
        echo "membership, Developer ID Application certificate, App Store Connect"
        echo "Notary API key, Team ID) and a worked example invocation."
    } >&2
    exit 78
fi

if [[ ! -f "$AC_API_KEY_PATH" ]]; then
    echo "release.sh: AC_API_KEY_PATH=$AC_API_KEY_PATH does not exist" >&2
    exit 78
fi

# 2. Verify the Developer ID identity is actually in the keychain. The
#    user passes the full quoted identity string, so an exact substring
#    match against `security find-identity` output is the right test.
if ! security find-identity -v -p codesigning | grep -qF "$DEVELOPER_ID_APPLICATION"; then
    {
        echo "release.sh: codesigning identity not found in keychain:"
        echo "  $DEVELOPER_ID_APPLICATION"
        echo
        echo "Available codesigning identities:"
        security find-identity -v -p codesigning | sed 's/^/  /'
    } >&2
    exit 78
fi

# 3. Layout helper + workspace version.
# shellcheck source=tools/_bundle_layout.sh
source "$ROOT/tools/_bundle_layout.sh"
VERSION=$(vetter_workspace_version)

echo "release.sh: building Vetter $VERSION for x86_64 + arm64 ..."

# 4. Build for both macOS arches in release mode. Cross-arch builds need
#    rustup-managed targets (`rustup target add ...`); Homebrew rust
#    ships only the host arch and will fail here with a clear rustc
#    "can't find crate `std`" error pointing at the missing toolchain.
for target in x86_64-apple-darwin aarch64-apple-darwin; do
    if ! cargo build --release --target "$target" -p vetterd -p vet; then
        {
            echo
            echo "release.sh: cargo build failed for $target."
            echo "Most common cause: the rustup target is not installed."
            echo "  rustup target add x86_64-apple-darwin aarch64-apple-darwin"
            echo
            echo "Distribution builds need rustup so we can target both"
            echo "Mac arches; Homebrew rust ships only the host arch."
        } >&2
        exit 1
    fi
done

# 5. Lipo the per-arch binaries into universal Mach-Os.
UNIV="target/universal/release"
mkdir -p "$UNIV"
for bin in vetterd vet; do
    lipo -create \
        "target/x86_64-apple-darwin/release/$bin" \
        "target/aarch64-apple-darwin/release/$bin" \
        -output "$UNIV/$bin"
    chmod +x "$UNIV/$bin"
done

# 6. Lay out Vetter.app from the universal binaries.
APP="target/Vetter.app"
vetter_layout_app "$UNIV" "$APP"

# 7. Sign every Mach-O inside the bundle, then the bundle itself, with
#    hardened runtime + secure timestamp + entitlements. Modern
#    notarisation rejects `--deep` and requires every nested binary to
#    carry its own signature, so we walk MacOS/* explicitly.
ENTITLEMENTS="$ROOT/vetterd/resources/vetterd.entitlements"
for bin in "$APP/Contents/MacOS/vet" "$APP/Contents/MacOS/vetterd"; do
    codesign --force --options runtime --timestamp \
        --entitlements "$ENTITLEMENTS" \
        --sign "$DEVELOPER_ID_APPLICATION" \
        "$bin"
done
codesign --force --options runtime --timestamp \
    --entitlements "$ENTITLEMENTS" \
    --sign "$DEVELOPER_ID_APPLICATION" \
    "$APP"

# 8. Cheap local verification before paying the notary submission
#    round-trip cost (a bad bundle here surfaces in seconds; a bad
#    bundle in notarisation surfaces in minutes).
codesign --verify --strict --verbose=2 "$APP"

# 9. Zip with ditto. `zip -r` strips metadata that codesign and the
#    notary service depend on; ditto -c -k --keepParent is the only
#    canonical path.
ZIP="target/Vetter-$VERSION.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

# 10. Submit to Apple's notary service and block until a decision lands.
#     If notarytool rejects, re-fetch the log with:
#       xcrun notarytool log <submission-id> \
#           --key "$AC_API_KEY_PATH" --key-id "$AC_API_KEY_ID" \
#           --issuer "$AC_API_KEY_ISSUER"
echo "release.sh: submitting to Apple notary service (this can take a few minutes) ..."
xcrun notarytool submit "$ZIP" \
    --key "$AC_API_KEY_PATH" \
    --key-id "$AC_API_KEY_ID" \
    --issuer "$AC_API_KEY_ISSUER" \
    --team-id "$TEAM_ID" \
    --wait

# 11. Staple the notarisation ticket onto the bundle so Gatekeeper
#     accepts it offline (e.g. the first time a new user opens the .app
#     without network).
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"

# 12. Re-zip with the stapled bundle so the artifact users actually
#     download has the staple embedded.
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

# 13. Compute the cask sha256 and print the publish-ready summary.
SHA256=$(shasum -a 256 "$ZIP" | awk '{print $1}')
cat <<EOF

release.sh: signed + notarised + stapled bundle ready.
  app:    $APP
  zip:    $ZIP
  sha256: $SHA256

Paste into the tap repo's Casks/vetter.rb (then commit + push):
  version "$VERSION"
  sha256 "$SHA256"

Canonical tap clone: git clone git@github.com:blevinstein/homebrew-vetter
Then attach $ZIP to a GitHub Release tagged v$VERSION on this repo.
See plans/Release.md for the full publish walkthrough.
EOF
