#!/usr/bin/env bash
# Render the approval window against a nested X server and capture it.
#
# Why this exists: scripted visual verification is otherwise impossible.
# On Wayland a client cannot raise or focus its own window without an
# activation token (plans/LinuxApp.md §5.6), so `vet daemon open` cannot
# bring the window forward and `spectacle -a` captures whatever *is*
# active instead. Full-screen capture then shows whatever is stacked on
# top. A nested X server has its own framebuffer, so `import -display`
# sees the window regardless of the real desktop's stacking order — and
# never touches the user's screen.
#
# It is also the only thing that exercises the **X11** path, which
# plans/LinuxApp.md §8 flags as validated nowhere: everything else in
# the Linux work has been Wayland-only.
#
# Development-only. Not part of the release tarball (Phase 6f).
#
# KNOWN LIMITATION (2026-09-09): under this rig the daemon accepts
# connections and answers the admin socket, but prompt-class requests do
# not reach the pending queue — `vet` blocks, `vet daemon list` reports
# none, and no audit entry is written. The same isolated daemon parks
# correctly with VETTERD_NOTIFIER=noop (which also means no window), so
# it is specific to the linux notifier on a private bus with no
# notification server to activate. Until that is root-caused this
# captures the window's *empty* state; see TODO.md. It still exercises
# GTK/X11 rendering, which is what it was built for.
#
# Requires Xephyr, dbus-run-session and ImageMagick (`import`). These
# are NOT in tools/install-deps.sh — that script declares what you need
# to *build* vetter, and these are only needed to look at it:
#   Fedora: sudo dnf install xorg-x11-server-Xephyr ImageMagick dbus-daemon
#   Debian: sudo apt-get install xserver-xephyr imagemagick dbus
#
# Usage:  tools/dev-window.sh [output.png]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DISPLAY_NUM="${DISPLAY_NUM:-:99}"
GEOMETRY="${GEOMETRY:-900x1000}"
OUT="${1:-/tmp/vetter-window.png}"

for need in Xephyr dbus-run-session import; do
    command -v "$need" >/dev/null 2>&1 || {
        echo "dev-window.sh: missing \`$need\` — see the header for install hints" >&2
        exit 1
    }
done
[ -x ./target/release/vetterd ] || {
    echo "dev-window.sh: build first — cargo build --release -p vetterd -p vet" >&2
    exit 1
}

# Socket lives under /run/user, never a long scratch path: `sun_path` is
# 108 bytes and the daemon refuses anything longer.
RUN_DIR="$(mktemp -d "/run/user/$(id -u)/vetdev.XXXXXX")"

cleanup() {
    # Deliberately NOT `pkill -f vetterd`: that pattern also matches this
    # script's own command line, and killing your own shell mid-cleanup
    # is a trap that has caught more than one agent working in this repo.
    # Key off a marker unique to the child we started.
    for p in $(pgrep -f 'release/vetterd' 2>/dev/null || true); do
        if grep -qa "VETTER_DEV_WINDOW=$$" "/proc/$p/environ" 2>/dev/null; then
            kill "$p" 2>/dev/null || true
        fi
    done
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null || true
    rm -rf "$RUN_DIR"
}
trap cleanup EXIT

setsid Xephyr "$DISPLAY_NUM" -screen "$GEOMETRY" -ac -noreset \
    >"$RUN_DIR/xephyr.log" 2>&1 &
XEPHYR_PID=$!
sleep 1

# Three things here are load-bearing and none are obvious:
#
#   dbus-run-session — the daemon takes a private session bus. Phase 6d
#     step 3 added a single-instance guard that (correctly) refuses to
#     start while another vetterd owns `dev.vetter.daemon`, so without
#     this the dev daemon exits 78 against the user's real one.
#   GTK_A11Y=none — on a private bus, GApplication registration fails
#     with "Could not activate remote peer 'org.a11y.atspi.Registry'"
#     and the window is *silently* never created. No error, no window.
#   GDK_BACKEND=x11 with WAYLAND_DISPLAY unset — otherwise GTK picks
#     Wayland and ignores the nested X server entirely.
env -u WAYLAND_DISPLAY \
    DISPLAY="$DISPLAY_NUM" \
    GDK_BACKEND=x11 \
    GTK_A11Y=none \
    VETTER_DEV_WINDOW="$$" \
    VETTERD_SOCKET="$RUN_DIR/v.sock" \
    VETTER_AUDIT_LOG="$RUN_DIR/audit.log" \
    setsid dbus-run-session -- ./target/release/vetterd \
    >"$RUN_DIR/daemon.log" 2>&1 &
sleep 3

if [ ! -S "$RUN_DIR/v.sock" ]; then
    echo "dev-window.sh: daemon did not bind its socket; log follows" >&2
    cat "$RUN_DIR/daemon.log" >&2
    exit 1
fi

VETTERD_SOCKET="$RUN_DIR/v.sock" "$REPO_ROOT/tools/park-spread.sh" || true
sleep 2

# Read the nested server's root window: the whole point, since it is
# unaffected by what is stacked on the real desktop.
import -display "$DISPLAY_NUM" -window root "$OUT"
echo "dev-window.sh: captured $OUT"
echo "  daemon log: $RUN_DIR/daemon.log (removed on exit — copy it now if needed)"
