#!/usr/bin/env bash
# tools/build-linux.sh — build the Linux release tarball.
#
# The Linux counterpart of tools/build-app.sh. Per plans/LinuxApp.md
# §4.3 the distribution mechanism for this milestone is a tarball plus
# `cargo install` — no .deb, no PPA, no COPR, no Flatpak — so this
# script produces the one artifact that story needs:
#
#   target/vetter-<version>-x86_64-linux.tar.gz
#
# Contents, unpacked into a single versioned directory:
#
#   bin/vet, bin/vetterd          the binaries
#   share/applications/...        desktop entry: gives the window and
#   share/icons/hicolor/...       its notifications a name and an icon
#   install.sh                    copies the above into ~/.local
#   _xdg_install.sh               shared with tools/install-desktop.sh
#   install-deps.sh               runtime dependency check
#   LICENSE, README.md
#
# Development-only tooling (dev-window.sh, park-spread.sh, the test
# harness) deliberately does not ship: the tarball's audience is
# installing a binary, not hacking on one.
#
# Usage:
#   tools/build-linux.sh                 build release + stage + tar
#   tools/build-linux.sh --no-build      stage from an existing build
#
# Output paths are printed at the end.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

DO_BUILD=1
case "${1:-}" in
    "")          ;;
    --no-build)  DO_BUILD=0 ;;
    -h|--help)
        awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"
        exit 0
        ;;
    *) echo "build-linux.sh: unknown argument: $1" >&2; exit 64;;
esac

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "build-linux.sh: this script only makes sense on Linux" >&2
    exit 64
fi

# Version comes from the same helper the macOS bundle uses, so the two
# artifacts can never disagree about what release they are from.
# _bundle_layout.sh only defines functions when sourced, and
# vetter_workspace_version reads the workspace Cargo.toml — nothing in
# it is macOS-specific.
# shellcheck source=tools/_bundle_layout.sh
source "$ROOT/tools/_bundle_layout.sh"
VERSION=$(vetter_workspace_version)
if [[ -z "$VERSION" ]]; then
    echo "build-linux.sh: could not read workspace version from Cargo.toml" >&2
    exit 1
fi

ARCH="$(uname -m)"
NAME="vetter-$VERSION-$ARCH-linux"
STAGE="target/$NAME"
TARBALL="target/$NAME.tar.gz"

if (( DO_BUILD )); then
    cargo build --release -p vetterd -p vet
fi

for bin in vet vetterd; do
    if [[ ! -x "target/release/$bin" ]]; then
        echo "build-linux.sh: target/release/$bin missing — run without --no-build" >&2
        exit 1
    fi
done

rm -rf "$STAGE"
mkdir -p "$STAGE/bin"

install -Dm755 target/release/vet     "$STAGE/bin/vet"
install -Dm755 target/release/vetterd "$STAGE/bin/vetterd"

install -Dm644 "share/applications/dev.vetter.daemon.desktop" \
    "$STAGE/share/applications/dev.vetter.daemon.desktop"
install -Dm644 "share/icons/hicolor/scalable/apps/dev.vetter.daemon.svg" \
    "$STAGE/share/icons/hicolor/scalable/apps/dev.vetter.daemon.svg"

# The installer users run is _dist_install.sh under its shipping name.
# _xdg_install.sh rides along so the tarball and the in-repo
# install-desktop.sh share one implementation of the entry/icon layout
# rather than two copies that drift.
install -Dm755 tools/_dist_install.sh "$STAGE/install.sh"
install -Dm644 tools/_xdg_install.sh  "$STAGE/_xdg_install.sh"

# install-deps.sh is self-contained (it references no repo paths) and
# is the single source of truth for system package names, so the
# tarball ships it verbatim for its runtime dependency check.
install -Dm755 tools/install-deps.sh "$STAGE/install-deps.sh"

install -Dm644 LICENSE   "$STAGE/LICENSE"
install -Dm644 README.md "$STAGE/README.md"

# --sort=name and a fixed owner keep the archive reproducible enough
# that rebuilding the same commit produces the same listing order.
tar --create --gzip \
    --file "$TARBALL" \
    --directory target \
    --sort=name \
    --owner=0 --group=0 --numeric-owner \
    "$NAME"

echo "built $TARBALL (version=$VERSION, arch=$ARCH)"
echo "  staged tree: $STAGE"
echo
echo "verify:"
echo "  tar -tzf $TARBALL"
echo "  tar -xzf $TARBALL -C /tmp && /tmp/$NAME/install.sh --prefix /tmp/vetter-test"
echo
echo "publish: see plans/Release.md §\"Linux / tarball\""
