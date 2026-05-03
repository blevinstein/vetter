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
# This script is the developer experience and intentionally does NOT
# notarise. For a Developer-ID signed + notarised + stapled bundle
# suitable for distribution (Homebrew cask, GitHub Release upload), use
# `tools/release.sh` and follow `plans/Release.md`.

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

# 2. Lay out the bundle via the shared helper (also used by release.sh)
#    so dev and release builds can never drift on bundle structure.
# shellcheck source=tools/_bundle_layout.sh
source "$ROOT/tools/_bundle_layout.sh"
VERSION=$(vetter_workspace_version)

APP="target/Vetter.app"
vetter_layout_app "target/$PROFILE" "$APP"

# 3. Ad-hoc code-sign so UNUserNotifications recognises the bundle.
#    `--deep` re-signs every Mach-O inside (vetterd + vet) so the
#    helper binary is also covered. For real distribution the
#    Developer-ID + notarisation path lives in `tools/release.sh`.
codesign --sign - --force --deep "$APP" >/dev/null

echo "built $APP (profile=$PROFILE, version=$VERSION)"
echo "  - $APP/Contents/MacOS/vetterd  (daemon, also CFBundleExecutable)"
echo "  - $APP/Contents/MacOS/vet      (CLI helper)"
echo "next:"
echo "  open $APP                                         # launch the menu-bar daemon"
echo "  export PATH=\"\$PWD/$APP/Contents/MacOS:\$PATH\"     # put vet on PATH"
echo
echo "for a notarised distribution build, see tools/release.sh + plans/Release.md"
