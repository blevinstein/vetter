#!/usr/bin/env bash
# tools/install-desktop.sh — install Vetter's desktop entry and icon
# into the user's XDG data dir, straight from a git checkout.
#
# This is the *development* entry point. It installs only the identity
# payload (desktop entry + icon), not the binaries, because a developer
# already runs those out of `target/release`. The Phase 6f release
# tarball ships the same payload plus the binaries through its own
# `install.sh`.
#
# Both are thin wrappers over `tools/_xdg_install.sh`, which owns the
# layout — see that file for why the desktop entry is what gives the
# GTK window and its notifications a name and an icon on Wayland
# (plans/LinuxApp.md §5.2).
#
# Unlike its sibling `install-deps.sh`, this installs by default rather
# than dry-running: everything it writes lives under the user's own
# $XDG_DATA_HOME, needs no sudo, and is undone by --uninstall.
#
# Usage:
#   tools/install-desktop.sh              install entry + icon, refresh caches
#   tools/install-desktop.sh --uninstall  remove both, refresh caches
#   tools/install-desktop.sh --check      exit 0 if installed, 1 if not

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"

# shellcheck source=tools/_xdg_install.sh
source "$REPO_ROOT/tools/_xdg_install.sh"

MODE=install
case "${1:-}" in
    "")           MODE=install ;;
    --uninstall)  MODE=uninstall ;;
    --check)      MODE=check ;;
    -h|--help)
        # Contiguous comment block from line 2 to the first non-comment
        # line, so this cannot rot as the header grows.
        awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"
        exit 0
        ;;
    *)            echo "install-desktop.sh: unknown argument '$1'" >&2; exit 2 ;;
esac

case "$MODE" in
    check)
        if ! vetter_xdg_check "$DATA_HOME"; then
            echo "install-desktop.sh: not installed; run tools/install-desktop.sh" >&2
            exit 1
        fi
        echo "install-desktop.sh: desktop entry and icon installed under $DATA_HOME"
        ;;
    install)
        vetter_xdg_install "$REPO_ROOT" "$DATA_HOME" || {
            echo "install-desktop.sh: source files missing from $REPO_ROOT/share" >&2
            exit 1
        }
        echo "install-desktop.sh: installed"
        echo "  $VETTER_DESKTOP_DST"
        echo "  $VETTER_ICON_DST"
        echo
        echo "Restart the daemon for a running window to pick up the icon:"
        echo "  vet daemon stop && vet daemon start"
        ;;
    uninstall)
        vetter_xdg_uninstall "$DATA_HOME"
        echo "install-desktop.sh: removed $VETTER_DESKTOP_DST and $VETTER_ICON_DST"
        ;;
esac
