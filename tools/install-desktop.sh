#!/usr/bin/env bash
# Install Vetter's desktop entry and icon into the user's XDG data dir.
#
# Why this exists: on Wayland a window's icon is not a window property.
# The compositor takes the `app_id` the toplevel advertises — GTK sets
# it from `Application::application_id`, which is `dev.vetter.daemon` —
# looks for a *installed* `<app_id>.desktop`, and resolves that entry's
# `Icon=` key through the icon theme. Running straight out of
# `target/release` installs neither, so KWin falls back to a generic
# placeholder glyph. Same lookup gives notifications their name and
# icon via the `desktop-entry` hint (plans/LinuxApp.md §5.2).
#
# Unlike its sibling `install-deps.sh`, this installs by default rather
# than dry-running: everything it writes lives under the user's own
# $XDG_DATA_HOME, needs no sudo, and is undone by --uninstall.
#
# This is a development-time convenience. Phase 6f's release tarball
# ships the same payload through its own `install.sh`; when that lands,
# this should become a thin wrapper over the shared logic rather than a
# second copy of it.
#
# Usage:
#   tools/install-desktop.sh              install entry + icon, refresh caches
#   tools/install-desktop.sh --uninstall  remove both, refresh caches
#   tools/install-desktop.sh --check      exit 0 if installed, 1 if not

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"

APP_ID="dev.vetter.daemon"
DESKTOP_SRC="$REPO_ROOT/share/applications/$APP_ID.desktop"
ICON_SRC="$REPO_ROOT/share/icons/hicolor/scalable/apps/$APP_ID.svg"
DESKTOP_DST="$DATA_HOME/applications/$APP_ID.desktop"
ICON_DST="$DATA_HOME/icons/hicolor/scalable/apps/$APP_ID.svg"

MODE=install
case "${1:-}" in
    "")           MODE=install ;;
    --uninstall)  MODE=uninstall ;;
    --check)      MODE=check ;;
    -h|--help)    sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'; exit 0 ;;
    *)            echo "install-desktop.sh: unknown argument '$1'" >&2; exit 2 ;;
esac

# Refresh the desktop database and icon cache. Both are best-effort:
# they are optimisations, and a missing cache only costs a slower
# lookup. `gtk4-update-icon-cache` *requires* an index.theme in the
# theme root, which a fresh per-user hicolor tree does not have — so
# seed one from the system theme when it is absent rather than letting
# the tool fail the script under `set -e`.
refresh_caches() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DATA_HOME/applications" 2>/dev/null || true
    fi
    local theme_root="$DATA_HOME/icons/hicolor"
    if [[ -d "$theme_root" && ! -f "$theme_root/index.theme" ]]; then
        if [[ -f /usr/share/icons/hicolor/index.theme ]]; then
            cp /usr/share/icons/hicolor/index.theme "$theme_root/index.theme"
        fi
    fi
    if [[ -f "$theme_root/index.theme" ]]; then
        for cache in gtk4-update-icon-cache gtk-update-icon-cache; do
            if command -v "$cache" >/dev/null 2>&1; then
                "$cache" -f -t "$theme_root" 2>/dev/null || true
                break
            fi
        done
    fi
}

case "$MODE" in
    check)
        missing=0
        [[ -f "$DESKTOP_DST" ]] || { echo "missing $DESKTOP_DST"; missing=1; }
        [[ -f "$ICON_DST" ]]    || { echo "missing $ICON_DST"; missing=1; }
        if (( missing )); then
            echo "install-desktop.sh: not installed; run tools/install-desktop.sh" >&2
            exit 1
        fi
        echo "install-desktop.sh: desktop entry and icon installed under $DATA_HOME"
        ;;
    install)
        [[ -f "$DESKTOP_SRC" ]] || { echo "install-desktop.sh: missing $DESKTOP_SRC" >&2; exit 1; }
        [[ -f "$ICON_SRC" ]]    || { echo "install-desktop.sh: missing $ICON_SRC" >&2; exit 1; }
        install -Dm644 "$DESKTOP_SRC" "$DESKTOP_DST"
        install -Dm644 "$ICON_SRC" "$ICON_DST"
        refresh_caches
        echo "install-desktop.sh: installed"
        echo "  $DESKTOP_DST"
        echo "  $ICON_DST"
        echo
        echo "Restart the daemon for a running window to pick up the icon:"
        echo "  vet daemon stop && vet daemon start"
        ;;
    uninstall)
        rm -f "$DESKTOP_DST" "$ICON_DST"
        refresh_caches
        echo "install-desktop.sh: removed $DESKTOP_DST and $ICON_DST"
        ;;
esac
