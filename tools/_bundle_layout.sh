# tools/_bundle_layout.sh — shared Vetter.app layout helper.
#
# Sourced by both `tools/build-app.sh` (dev / ad-hoc signed) and
# `tools/release.sh` (Developer-ID signed + notarised). Owns the bundle
# directory layout and Info.plist rendering so the two callers can never
# drift on what a "Vetter.app" looks like — codesign and the AppKit
# pitfalls in plans/MacOSApp.md make a single layout source important.
#
# Callers must have set `ROOT` (repo root) before sourcing.
#
# Provided functions:
#   vetter_workspace_version
#       echoes the workspace version from the top-level Cargo.toml.
#   vetter_layout_app SRC_DIR APP_PATH
#       Lays out APP_PATH as a Vetter.app bundle, copying `vetterd` and
#       `vet` from SRC_DIR into Contents/MacOS and rendering Info.plist
#       from the template (substitutes __VERSION__ from $VERSION).
#       Removes APP_PATH if it already exists. Does NOT codesign — the
#       caller owns that step.

vetter_workspace_version() {
    # Top-level workspace.package.version sits at the start of Cargo.toml.
    grep '^version' "$ROOT/Cargo.toml" | head -1 | sed -E 's/.*"([^"]+)".*/\1/'
}

vetter_layout_app() {
    local src_dir="$1"
    local app="$2"

    if [[ -z "${VERSION:-}" ]]; then
        echo "vetter_layout_app: VERSION must be set in the caller" >&2
        return 1
    fi

    local vetterd_bin="$src_dir/vetterd"
    local vet_bin="$src_dir/vet"
    for bin in "$vetterd_bin" "$vet_bin"; do
        if [[ ! -x "$bin" ]]; then
            echo "vetter_layout_app: $bin missing or not executable" >&2
            return 1
        fi
    done

    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS"
    mkdir -p "$app/Contents/Resources"
    cp "$vetterd_bin" "$app/Contents/MacOS/vetterd"
    cp "$vet_bin"     "$app/Contents/MacOS/vet"
    chmod +x "$app/Contents/MacOS/vetterd" "$app/Contents/MacOS/vet"

    sed "s/__VERSION__/$VERSION/g" \
        "$ROOT/vetterd/resources/Info.plist.template" \
        > "$app/Contents/Info.plist"
}
