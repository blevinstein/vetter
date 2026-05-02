#!/usr/bin/env bash
# tools/build-app.sh — build the Vetter.app bundle for local dev.
#
# Wraps the cargo-built `vetterd` binary in a minimal `.app` bundle
# with the Phase 4 Info.plist (LSUIElement + UNUserNotifications),
# ad-hoc code-signs it (`codesign --sign -`), and prints the path so
# the manual smoke test in `plans/Phase4Notes.md` can `open` it.
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

# 1. Build vetterd.
cargo build -p vetterd $CARGO_FLAG

BIN_PATH="target/$PROFILE/vetterd"
if [[ ! -x "$BIN_PATH" ]]; then
    echo "build-app.sh: $BIN_PATH missing — cargo build failed?" >&2
    exit 1
fi

# 2. Lay out the bundle.
APP="target/Vetter.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"
cp "$BIN_PATH" "$APP/Contents/MacOS/vetterd"
chmod +x "$APP/Contents/MacOS/vetterd"

# 3. Render Info.plist from the template.
VERSION=$(grep '^version' Cargo.toml | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
sed "s/__VERSION__/$VERSION/g" \
    vetterd/resources/Info.plist.template \
    > "$APP/Contents/Info.plist"

# 4. Ad-hoc code-sign so UNUserNotifications recognises the bundle.
#    For real distribution: replace `-` with a Developer ID identity
#    and add notarisation (a Phase 4 follow-up PR).
codesign --sign - --force --deep "$APP" >/dev/null

echo "built $APP (profile=$PROFILE, version=$VERSION)"
echo "next: open $APP   # or run target/Vetter.app/Contents/MacOS/vetterd directly"
