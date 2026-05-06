#!/usr/bin/env bash
# tools/build-icons.sh — regenerate the macOS icon assets shipped with
# the Vetter.app bundle.
#
# Inputs:
#   assets/vetter-logo.png        full-colour 1254×1254 source for the
#                                  Finder / Dock icon (CFBundleIconFile)
#   assets/vetter-logo-mono.svg   hand-crafted monochrome variant for
#                                  the menu-bar status item (NSImage
#                                  template image; system tints to
#                                  match the menu bar appearance)
#
# Outputs (committed to the repo so the build doesn't depend on the
# generator tools at all — only on _bundle_layout.sh copying these
# files into Vetter.app/Contents/Resources/):
#   assets/generated/AppIcon.icns      multi-resolution Apple icns
#                                       built by `iconutil` from a
#                                       sips-generated .iconset
#   assets/generated/StatusItem.png    256×256 monochrome PNG with
#                                       transparent background; loaded
#                                       at runtime via NSImage and
#                                       scaled to the menu-bar point
#                                       size by AppKit. setTemplate(true)
#                                       handles the dark/light tint.
#
# Tools used (all shipped with the macOS Command Line Tools, which
# `cargo` already requires — no Homebrew dependency):
#   sips        rasterise + resize PNGs
#   iconutil    pack a .iconset directory into an .icns
#   swift       run tools/_render_template_png.swift, which uses
#               Cocoa (NSImage + a CGContext explicitly initialised
#               with an alpha channel) to render the monochrome SVG
#               into a transparent PNG. We rolled our own renderer
#               here because qlmanage's WebKit-based SVG generator
#               flattens transparent SVG backgrounds to opaque white,
#               which then renders as a solid square in the menu bar
#               once the result is marked as a template image.
#
# Re-run this script after editing either source asset, then commit
# the regenerated outputs alongside the source change. CI / Linux
# packagers never invoke this script — they consume the committed
# outputs directly.

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "build-icons.sh: this script only runs on macOS (needs sips/iconutil/qlmanage)" >&2
    exit 64
fi

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

SRC_PNG="$ROOT/assets/vetter-logo.png"
SRC_SVG="$ROOT/assets/vetter-logo-mono.svg"
OUT="$ROOT/assets/generated"

for f in "$SRC_PNG" "$SRC_SVG"; do
    if [[ ! -f "$f" ]]; then
        echo "build-icons.sh: missing source asset: $f" >&2
        exit 78
    fi
done

mkdir -p "$OUT"

# 1. Pack the colour PNG into an Apple .icns. Sizes follow Apple's
#    documented "iconset" layout: every standard size from 16pt up to
#    512pt, each with a matching @2x retina representation, named so
#    iconutil recognises them. 1024×1024 covers Finder Get Info and
#    the Dock at 4x scaling.
ICONSET_DIR="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET_DIR"
trap 'rm -rf "$(dirname "$ICONSET_DIR")"' EXIT

# `name@sizeAtPoint`. The first column is the pixel dimension we
# downsample to, the second is the iconutil-required filename.
specs=(
    "16   icon_16x16.png"
    "32   icon_16x16@2x.png"
    "32   icon_32x32.png"
    "64   icon_32x32@2x.png"
    "128  icon_128x128.png"
    "256  icon_128x128@2x.png"
    "256  icon_256x256.png"
    "512  icon_256x256@2x.png"
    "512  icon_512x512.png"
    "1024 icon_512x512@2x.png"
)
for spec in "${specs[@]}"; do
    size="${spec%% *}"
    name="${spec##* }"
    sips -z "$size" "$size" "$SRC_PNG" --out "$ICONSET_DIR/$name" >/dev/null
done

iconutil -c icns "$ICONSET_DIR" -o "$OUT/AppIcon.icns"

# 2. Rasterise the monochrome SVG into a transparent PNG via
#    tools/_render_template_png.swift (see the script header for the
#    qlmanage-vs-NSImage rationale). Output size is 256×256 — well
#    above the largest menu-bar point size we'd ever ask AppKit to
#    render at, so the runtime NSImage scaling produces crisp results
#    on retina displays without us shipping multiple representations.
swift "$ROOT/tools/_render_template_png.swift" \
    "$SRC_SVG" "$OUT/StatusItem.png" 256

echo "build-icons.sh: regenerated"
echo "  $OUT/AppIcon.icns"
echo "  $OUT/StatusItem.png"
echo
echo "commit both files alongside any change to the source assets"
echo "(assets/vetter-logo.png / assets/vetter-logo-mono.svg)."
