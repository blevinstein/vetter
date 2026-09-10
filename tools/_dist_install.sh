#!/usr/bin/env bash
# install.sh — install Vetter from the release tarball.
#
# Copies `vet` and `vetterd` into ~/.local/bin and the desktop entry
# and icon into ~/.local/share, then refreshes the desktop database and
# icon cache. Everything lands under your home directory: this never
# calls sudo and never writes outside $PREFIX and $XDG_DATA_HOME.
#
# Usage:
#   ./install.sh                     install into ~/.local
#   ./install.sh --prefix ~/opt      install into a different prefix
#   ./install.sh --uninstall         remove what this script installed
#   ./install.sh --check             exit 0 if installed, 1 if not
#
# Uninstall is offered because writing into a user's ~/.local without
# an undo is poor citizenship, and because a tarball has no package
# manager to do it for you.
#
# NOTE for maintainers: the copy of this that users run is staged into
# the tarball by tools/build-linux.sh; the source of truth is
# tools/_dist_install.sh in the vetter repo. It shares its desktop-entry
# logic with the development-time tools/install-desktop.sh through
# _xdg_install.sh, which is staged alongside it.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"

MODE=install
while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)    PREFIX="${2:?--prefix needs a path}"; shift 2;;
        --uninstall) MODE=uninstall; shift;;
        --check)     MODE=check; shift;;
        -h|--help)
            awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"
            exit 0
            ;;
        *) echo "install.sh: unknown argument: $1" >&2; exit 64;;
    esac
done

# shellcheck source=_xdg_install.sh
source "$HERE/_xdg_install.sh"

BINARIES=(vet vetterd)

case "$MODE" in
    check)
        missing=0
        for b in "${BINARIES[@]}"; do
            [[ -x "$PREFIX/bin/$b" ]] || { echo "missing $PREFIX/bin/$b"; missing=1; }
        done
        vetter_xdg_check "$DATA_HOME" || missing=1
        if (( missing )); then
            echo "install.sh: not fully installed" >&2
            exit 1
        fi
        echo "install.sh: vetter is installed under $PREFIX and $DATA_HOME"
        ;;

    install)
        # Runtime dependencies only. Someone installing a compiled
        # binary must not be told to install a compiler or -devel
        # packages; install-deps.sh models that distinction and is the
        # single place package names are written down, so it ships here
        # rather than this script growing a second list that drifts.
        if [[ -x "$HERE/install-deps.sh" ]]; then
            if ! "$HERE/install-deps.sh" --check --runtime >/dev/null 2>&1; then
                echo "install.sh: some runtime libraries are missing:"
                "$HERE/install-deps.sh" --runtime || true
                echo
                echo "  install them with: $HERE/install-deps.sh --run --runtime"
                echo "  (continuing — vetter is installed but will not start until they are present)"
                echo
            fi
        fi

        for b in "${BINARIES[@]}"; do
            [[ -f "$HERE/bin/$b" ]] || { echo "install.sh: missing $HERE/bin/$b" >&2; exit 1; }
            install -Dm755 "$HERE/bin/$b" "$PREFIX/bin/$b"
        done

        vetter_xdg_install "$HERE" "$DATA_HOME" || {
            echo "install.sh: desktop entry or icon missing from the tarball" >&2
            exit 1
        }

        echo "install.sh: installed"
        for b in "${BINARIES[@]}"; do echo "  $PREFIX/bin/$b"; done
        echo "  $VETTER_DESKTOP_DST"
        echo "  $VETTER_ICON_DST"
        echo

        case ":$PATH:" in
            *":$PREFIX/bin:"*) ;;
            *)
                echo "  NOTE: $PREFIX/bin is not on your \$PATH. Add it:"
                echo "    export PATH=\"$PREFIX/bin:\$PATH\""
                echo
                ;;
        esac

        echo "next:"
        echo "  vet daemon start"
        echo "  vet curl https://example.com/    # approve or reject from the banner"
        echo
        echo "If a daemon is already running it is still on the old binary;"
        echo "restart it to pick this one up:"
        echo "  vet daemon stop && vet daemon start"
        ;;

    uninstall)
        # The autostart entry is deliberately NOT removed here. It is a
        # different file with a different owner: `vet daemon autostart
        # enable` writes it, and deleting it behind the daemon's back
        # would leave settings.yaml claiming autostart is on. Warn
        # instead, because leaving it while removing the binary it
        # points at is exactly the stale-Exec case Phase 6e made
        # `vet doctor` report.
        autostart_entry="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/vetter.desktop"
        if [[ -f "$autostart_entry" ]]; then
            echo "install.sh: NOTE — autostart is still enabled:"
            echo "    $autostart_entry"
            echo "  It points at a binary this uninstall removes. Disable it first:"
            echo "    vet daemon autostart disable"
            echo
        fi

        for b in "${BINARIES[@]}"; do
            rm -f "$PREFIX/bin/$b"
        done
        vetter_xdg_uninstall "$DATA_HOME"

        echo "install.sh: removed"
        for b in "${BINARIES[@]}"; do echo "  $PREFIX/bin/$b"; done
        echo "  $VETTER_DESKTOP_DST"
        echo "  $VETTER_ICON_DST"
        echo
        echo "Left alone (yours, not the installer's):"
        echo "  ~/.vet/                       allowlist, known hosts, settings"
        echo "  \${XDG_STATE_HOME:-~/.local/state}/vetter/   audit log"
        ;;
esac
