#!/usr/bin/env bash
# tools/build-app.sh — build the Vetter.app bundle for local dev.
#
# Builds both `vetterd` (the daemon, also the bundle's main
# executable) and `vet` (the CLI users invoke), packages both into a
# minimal `.app` bundle with the Phase 4 Info.plist
# (LSUIElement + UNUserNotifications), ad-hoc code-signs the bundle
# (`codesign --sign -`), and prints the path so the manual smoke
# test in `plans/MacOSApp.md` can `open` it.
#
# Both binaries live in `Contents/MacOS/`. The bundle's
# `CFBundleExecutable` is `vetterd`, so `open Vetter.app` still
# launches the daemon; `vet` rides along as a helper so a developer
# can put one directory on PATH and have everything they need.
#
# Usage:
#   tools/build-app.sh           # debug build
#   tools/build-app.sh --release # release build
#
# Output: target/Vetter.app
#
# Real notarised/distribution builds are a follow-up; this script is
# the one-line developer experience and intentionally does not try to
# notarise.

set -euo pipefail

PROFILE="${1:-debug}"
case "$PROFILE" in
    --release|release) PROFILE=release; CARGO_FLAG="--release";;
    --debug|debug)     PROFILE=debug;   CARGO_FLAG="";;
    *)                 echo "usage: $0 [--release|--debug]" >&2; exit 64;;
esac

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-app.sh: this script only makes sense on macOS" >&2
    exit 64
fi

# 1. Build both workspace binaries in one cargo invocation.
cargo build -p vetterd -p vet $CARGO_FLAG

VETTERD_BIN="target/$PROFILE/vetterd"
VET_BIN="target/$PROFILE/vet"
for bin in "$VETTERD_BIN" "$VET_BIN"; do
    if [[ ! -x "$bin" ]]; then
        echo "build-app.sh: $bin missing — cargo build failed?" >&2
        exit 1
    fi
done

# 2. Lay out the bundle.
APP="target/Vetter.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"
cp "$VETTERD_BIN" "$APP/Contents/MacOS/vetterd"
cp "$VET_BIN"     "$APP/Contents/MacOS/vet"
chmod +x "$APP/Contents/MacOS/vetterd" "$APP/Contents/MacOS/vet"

# 3. Render Info.plist from the template.
VERSION=$(grep '^version' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
sed "s/__VERSION__/$VERSION/g" \
    vetterd/resources/Info.plist.template \
    > "$APP/Contents/Info.plist"

# 4. Ad-hoc code-sign so UNUserNotifications recognises the bundle.
#    `--deep` re-signs every Mach-O inside (vetterd + vet) so the
#    helper binary is also covered. For real distribution: replace
#    `-` with a Developer ID identity and add notarisation (a Phase
#    4 follow-up PR).
codesign --sign - --force --deep "$APP" >/dev/null

echo "built $APP (profile=$PROFILE, version=$VERSION)"
echo "  - $APP/Contents/MacOS/vetterd  (daemon, also CFBundleExecutable)"
echo "  - $APP/Contents/MacOS/vet      (CLI helper)"
echo "next:"
echo "  open $APP                                         # launch the menu-bar daemon"
echo "  export PATH=\"\$PWD/$APP/Contents/MacOS:\$PATH\"     # put vet on PATH"
