#!/usr/bin/env bash
# tools/install-deps.sh — vetter's system dependencies, and the single
# place their package names are written down.
#
# Cargo has no native mechanism for system packages: there is no
# `requirements.txt` equivalent, and `cargo install` deliberately has
# no pre-install hook that could invoke a package manager. For a
# security tool that absence is a feature — a crate able to run
# `sudo dnf` at build time would be exactly the supply-chain problem
# vetter exists to catch. So the dependency list lives here, in a
# script the user runs knowingly, and every other place that used to
# restate package names now points at it.
#
# Usage:
#   tools/install-deps.sh            print the install command; install nothing
#   tools/install-deps.sh --run      print it, then run it (uses sudo)
#   tools/install-deps.sh --check    exit 0 if all present, 1 if not
#   tools/install-deps.sh --runtime  operate on runtime deps, not build deps
#   tools/install-deps.sh --test     operate on test deps
#
# `--check` is machine-readable: one `missing <probe> <package>` record
# per line on stdout, human commentary on stderr, and an exit code that
# means something. That is what the CI job consumes, and what the Linux
# `vet doctor` rows in Phase 6e should shell out to rather than
# re-deriving the list. (Not wired to `vet doctor` yet — 6e's job.)
#
# This script never calls sudo unless you pass --run, and prints the
# exact command before running it.

set -euo pipefail

# ── Dependency table ────────────────────────────────────────────────
#
# THE single source of truth. Every mode below, the CI job, the
# per-distro table in plans/LinuxApp.md §9, and Phase 6f's packaging
# metadata derive from these rows. Add a system dependency here and
# nowhere else.
#
# Columns (whitespace-separated; `note` runs to end of line):
#   kind     build | runtime | test
#   probe    how to detect presence, distro-independent:
#              pkgconfig:<module>   pkg-config --exists <module>
#              command:<binary>     binary on PATH
#              lib:<soname>         shared library known to ldconfig
#   fedora   package name under dnf   (Fedora / RHEL / Nobara)
#   debian   package name under apt   (Debian / Ubuntu)
#   note     why it is needed
#
# `test` is the third kind: needed to RUN THE TEST SUITE, but by
# neither a build nor an installed binary. It exists because
# vetterd/tests/notifier_dbus_e2e.rs stands up a private session bus to
# drive the real LinuxNotifier end to end, and a dependency that only
# `cargo test` needs must not be inflicted on someone installing the
# Phase 6f tarball. Declaring it here rather than inlining an
# `apt-get install` in the workflow keeps this table's promise: package
# names live in exactly one place.
#
# build vs runtime is a real distinction, not bookkeeping. Build deps
# are needed to COMPILE from source; runtime deps are what an already
# built binary needs to RUN. The Phase 6f tarball audience needs only
# the runtime set, and telling them to install a compiler toolchain
# they will never invoke would be wrong. Nothing consumes the runtime
# rows yet — they are modelled now so 6f's install.sh and the RPM
# spec / debian control generate from this table instead of inventing
# a second list that drifts from it.
#
# Deliberately NOT here: dbus development headers. zbus is pure Rust,
# there is no dbus crate and no C -sys crate that links against
# libdbus, and Phases 6b and 6c were built and smoke-tested against a
# live session bus on a machine with no libdbus-1 dev headers at all.
# `dbus-devel` / `libdbus-1-dev` appeared in several documents as a
# leftover from the deleted plans/UbuntuApp.md, which assumed a
# libdbus-based implementation that was never written.
deps_table() {
    cat <<'TABLE'
build   pkgconfig:gtk4       gtk4-devel          libgtk-4-dev      GTK4 approval window (Phase 6d)
build   pkgconfig:glib-2.0   glib2-devel         libglib2.0-dev    GLib main loop; arrives via gtk4 but named explicitly
build   command:pkg-config   pkgconf-pkg-config  pkg-config        used by the gtk4 crate's build script to locate the above
build   command:cc           gcc                 build-essential   C compiler the gtk4 build script shells out to
runtime lib:libgtk-4.so.1    gtk4                libgtk-4-1        GTK4 shared library
runtime lib:libglib-2.0.so.0 glib2               libglib2.0-0      GLib shared library
test    command:dbus-daemon  dbus-daemon         dbus-daemon       private session bus for the notifier end-to-end smoke
TABLE
}

# ── Argument parsing ────────────────────────────────────────────────

MODE=print
KIND=build

while [[ $# -gt 0 ]]; do
    case "$1" in
        --run)     MODE=run;;
        --check)   MODE=check;;
        --runtime) KIND=runtime;;
        --build)   KIND=build;;
        --test)    KIND=test;;
        -h|--help)
            # Contiguous comment block from line 2 to the first
            # non-comment line, so this can't rot as the header grows.
            awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"
            exit 0
            ;;
        *)
            echo "install-deps.sh: unknown argument: $1" >&2
            echo "usage: $0 [--run|--check] [--build|--runtime|--test]" >&2
            exit 64
            ;;
    esac
    shift
done

# ── Platform detection ──────────────────────────────────────────────

# macOS needs nothing beyond the Xcode command line tools, which
# already provide cc and pkg-config. The AppKit UI links frameworks
# that ship with the OS, so there is no Homebrew formula to install.
if [[ "$(uname -s)" == "Darwin" ]]; then
    echo "install-deps.sh: macOS needs no extra packages."
    echo "  The Xcode command line tools supply the C toolchain;"
    echo "  the AppKit UI links frameworks that ship with the OS."
    echo "  If \`cc\` is missing, run: xcode-select --install"
    exit 0
fi

# Distro family, resolved through ID then ID_LIKE so derivatives land
# on their parent (Nobara declares ID=nobara, ID_LIKE="rhel centos
# fedora"; Mint and Pop!_OS similarly resolve to debian/ubuntu).
FAMILY=unknown
if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091  # runtime file, not shipped in-repo
    . /etc/os-release
    # shellcheck disable=SC2086  # ID_LIKE is space-separated; splitting is the point
    for id in ${ID:-} ${ID_LIKE:-}; do
        case "$id" in
            fedora|rhel|centos|rocky|almalinux) FAMILY=fedora; break;;
            debian|ubuntu)                      FAMILY=debian; break;;
        esac
    done
fi

case "$FAMILY" in
    fedora) INSTALL_CMD="sudo dnf install -y";     PKG_COL=3;;
    debian) INSTALL_CMD="sudo apt-get install -y"; PKG_COL=4;;
    *)      INSTALL_CMD="";                        PKG_COL=0;;
esac

# ── Probing ─────────────────────────────────────────────────────────

# Answer "is this dependency already present?" without knowing the
# distro — probes are portable even when package names are not, which
# is why --check works on distros this script cannot install for.
probe_satisfied() {
    local probe="$1"
    local kind="${probe%%:*}"
    local arg="${probe#*:}"
    local cache
    case "$kind" in
        pkgconfig) command -v pkg-config >/dev/null 2>&1 && pkg-config --exists "$arg";;
        command)   command -v "$arg" >/dev/null 2>&1;;
        lib)
            command -v ldconfig >/dev/null 2>&1 || return 1
            # Capture before matching rather than piping into `grep -q`.
            # `ldconfig -p` emits thousands of lines, so a `grep -q` that
            # exits on the first match can SIGPIPE the writer; under
            # `set -o pipefail` that surfaces as a failed pipeline even
            # though the match succeeded, and whether it happens depends
            # on pipe buffering. This form has no such race.
            cache="$(ldconfig -p 2>/dev/null || true)"
            [[ "$cache" == *"$arg"* ]]
            ;;
        *)         return 1;;
    esac
}

# Collect the rows of interest into parallel arrays. Fed by a heredoc
# redirect rather than a pipe so the loop runs in this shell and the
# arrays survive it.
MISSING_PROBES=()
MISSING_PKGS=()
MISSING_NOTES=()
TOTAL=0

while read -r kind probe fedora debian note; do
    [[ "$kind" == "$KIND" ]] || continue
    TOTAL=$((TOTAL + 1))
    if probe_satisfied "$probe"; then
        continue
    fi
    case "$PKG_COL" in
        3) pkg="$fedora";;
        4) pkg="$debian";;
        *) pkg="";;
    esac
    MISSING_PROBES+=("$probe")
    MISSING_PKGS+=("$pkg")
    MISSING_NOTES+=("$note")
done <<<"$(deps_table)"

# ── Modes ───────────────────────────────────────────────────────────

if [[ ${#MISSING_PROBES[@]} -eq 0 ]]; then
    case "$MODE" in
        check) echo "install-deps.sh: all $TOTAL $KIND dependencies present" >&2;;
        *)     echo "install-deps.sh: all $TOTAL $KIND dependencies already present; nothing to do";;
    esac
    exit 0
fi

# Unknown distro: probes still told us what is missing, but we refuse
# to guess package names. Naming the pkg-config modules is more useful
# than a wrong `dnf install` line.
if [[ "$FAMILY" == unknown ]]; then
    {
        echo "install-deps.sh: unrecognised distribution; cannot map to package names."
        echo "The following $KIND dependencies are missing. Install your distro's"
        echo "packages providing them:"
        for i in "${!MISSING_PROBES[@]}"; do
            printf '  %-24s %s\n' "${MISSING_PROBES[$i]}" "${MISSING_NOTES[$i]}"
        done
    } >&2
    if [[ "$MODE" == check ]]; then
        for i in "${!MISSING_PROBES[@]}"; do
            echo "missing ${MISSING_PROBES[$i]} -"
        done
    fi
    exit 1
fi

case "$MODE" in
    check)
        # stdout: one record per missing dep, for machines.
        for i in "${!MISSING_PROBES[@]}"; do
            echo "missing ${MISSING_PROBES[$i]} ${MISSING_PKGS[$i]}"
        done
        # stderr: the human explanation.
        echo "install-deps.sh: ${#MISSING_PROBES[@]} of $TOTAL $KIND dependencies missing;" \
             "run tools/install-deps.sh --run" >&2
        exit 1
        ;;
    print|run)
        echo "install-deps.sh: detected $FAMILY-family distribution (${ID:-unknown})"
        echo "Missing $KIND dependencies:"
        for i in "${!MISSING_PROBES[@]}"; do
            printf '  %-20s %s\n' "${MISSING_PKGS[$i]}" "${MISSING_NOTES[$i]}"
        done
        echo
        echo "$INSTALL_CMD ${MISSING_PKGS[*]}"
        if [[ "$MODE" == print ]]; then
            echo
            echo "(nothing installed — re-run with --run to execute the command above)"
            exit 0
        fi
        echo
        # shellcheck disable=SC2086  # deliberate word-splitting of the command
        exec $INSTALL_CMD "${MISSING_PKGS[@]}"
        ;;
esac
