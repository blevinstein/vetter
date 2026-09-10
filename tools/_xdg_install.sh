# tools/_xdg_install.sh — shared XDG desktop-entry + icon installer.
#
# Sourced by both `tools/install-desktop.sh` (development, straight out
# of a git checkout) and the `install.sh` that ships inside the Phase 6f
# release tarball. Owns *what* gets written where, so the two entry
# points can never drift on the layout — the same reason
# `_bundle_layout.sh` exists on the macOS side.
#
# Why any of this is needed: on Wayland a window's icon is not a window
# property. The compositor takes the `app_id` the toplevel advertises —
# GTK sets it from `Application::application_id`, which is
# `dev.vetter.daemon` — looks for an *installed* `<app_id>.desktop`, and
# resolves that entry's `Icon=` key through the icon theme. Nothing
# installed means a generic placeholder glyph. The same lookup gives
# notifications their name and icon via the `desktop-entry` hint
# (plans/LinuxApp.md §5.2).
#
# Everything here writes under the caller's own $XDG_DATA_HOME and needs
# no sudo.
#
# Provided:
#   VETTER_APP_ID
#       The desktop-entry basename and GTK application id.
#   vetter_xdg_paths SRC_ROOT DATA_HOME
#       Sets VETTER_DESKTOP_SRC / _ICON_SRC / _DESKTOP_DST / _ICON_DST.
#       SRC_ROOT is a tree containing share/applications/... and
#       share/icons/... — a git checkout and an unpacked tarball both
#       satisfy that, which is what lets one implementation serve both.
#   vetter_xdg_install SRC_ROOT DATA_HOME
#   vetter_xdg_uninstall DATA_HOME
#   vetter_xdg_check DATA_HOME      (0 = installed, 1 = not)
#   vetter_xdg_refresh_caches DATA_HOME
#
# Deliberately NOT touched here: ~/.config/autostart/vetter.desktop.
# That is a *different* file with a different job — Phase 6e writes it
# to start the daemon at login, and it is owned by
# `vet daemon autostart enable/disable`. Colliding the two would make
# uninstalling the identity entry silently disable autostart, or worse,
# make `--uninstall` delete a file the user asked the daemon to manage.

VETTER_APP_ID="dev.vetter.daemon"

vetter_xdg_paths() {
    local src_root="$1"
    local data_home="$2"
    VETTER_DESKTOP_SRC="$src_root/share/applications/$VETTER_APP_ID.desktop"
    VETTER_ICON_SRC="$src_root/share/icons/hicolor/scalable/apps/$VETTER_APP_ID.svg"
    VETTER_DESKTOP_DST="$data_home/applications/$VETTER_APP_ID.desktop"
    VETTER_ICON_DST="$data_home/icons/hicolor/scalable/apps/$VETTER_APP_ID.svg"
}

# Refresh the desktop database and icon cache. Both are best-effort:
# they are optimisations, and a missing cache only costs a slower
# lookup. `gtk4-update-icon-cache` *requires* an index.theme in the
# theme root, which a fresh per-user hicolor tree does not have — so
# seed one from the system theme when it is absent rather than letting
# the tool fail the script under `set -e`.
vetter_xdg_refresh_caches() {
    local data_home="$1"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$data_home/applications" 2>/dev/null || true
    fi
    local theme_root="$data_home/icons/hicolor"
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

vetter_xdg_install() {
    local src_root="$1"
    local data_home="$2"
    vetter_xdg_paths "$src_root" "$data_home"

    local missing=0
    [[ -f "$VETTER_DESKTOP_SRC" ]] || { echo "missing $VETTER_DESKTOP_SRC" >&2; missing=1; }
    [[ -f "$VETTER_ICON_SRC" ]]    || { echo "missing $VETTER_ICON_SRC" >&2; missing=1; }
    (( missing )) && return 1

    install -Dm644 "$VETTER_DESKTOP_SRC" "$VETTER_DESKTOP_DST"
    install -Dm644 "$VETTER_ICON_SRC" "$VETTER_ICON_DST"
    vetter_xdg_refresh_caches "$data_home"
}

vetter_xdg_uninstall() {
    local data_home="$1"
    vetter_xdg_paths "/nonexistent" "$data_home"
    rm -f "$VETTER_DESKTOP_DST" "$VETTER_ICON_DST"
    vetter_xdg_refresh_caches "$data_home"
}

vetter_xdg_check() {
    local data_home="$1"
    vetter_xdg_paths "/nonexistent" "$data_home"
    local missing=0
    [[ -f "$VETTER_DESKTOP_DST" ]] || { echo "missing $VETTER_DESKTOP_DST"; missing=1; }
    [[ -f "$VETTER_ICON_DST" ]]    || { echo "missing $VETTER_ICON_DST"; missing=1; }
    return $missing
}
