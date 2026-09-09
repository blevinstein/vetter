#!/usr/bin/env bash
# Park the approval-card catalogue against a running daemon.
#
# Each request exercises a distinct element of plans/ApprovalUI.md, so
# the window shows every row type, tone and wrapper at once. This is the
# fixture set for the §7 manual smoke, for comparing against the macOS
# popover (plans/LinuxApp.md §6h), and for tools/dev-window.sh.
#
# It has been reconstructed by hand in four separate phases; this exists
# so the fifth time is a command rather than an archaeology exercise.
#
# Honours $VETTERD_SOCKET — both `vet` and `vetterd` resolve it through
# `vetter_core::default_socket_path`, so this can target an isolated dev
# daemon instead of the user's own. Set $VET to pick a binary; otherwise
# a `vet` on PATH wins, falling back to ./target/release/vet.
#
# Every request blocks until resolved, so each is backgrounded. Nothing
# here reaches the network: the hosts are .example names that do not
# resolve, and approving one just means curl fails at DNS.
set -euo pipefail

VET="${VET:-}"
if [ -z "$VET" ]; then
    if command -v vet >/dev/null 2>&1; then
        VET="$(command -v vet)"
    elif [ -x "./target/release/vet" ]; then
        VET="./target/release/vet"
    else
        echo "park-spread.sh: no \`vet\` on PATH and no ./target/release/vet" >&2
        echo "  build it first: cargo build --release -p vetterd -p vet" >&2
        exit 1
    fi
fi

FIX="$(mktemp -d)"
# The Open-file suppression rule is only *visible* as a difference, so
# the spread deliberately contains both cases: one path that exists and
# one that does not. A FileWrite target normally does not exist yet,
# which is exactly the case the rule is for.
printf 'vetter upload fixture\n' > "$FIX/upload.bin"   # exists  -> button OFFERED
MISSING="$FIX/never-written.bin"                        # absent  -> button SUPPRESSED
rm -f "$MISSING"

# Staggered, not fired in a batch: requests submitted together right
# after daemon startup have been observed to go missing, and the
# notifier coalesces anyway (only the empty -> non-empty transition
# raises a banner), so a burst tells you nothing extra.
park() {
    setsid "$@" >/dev/null 2>&1 &
    sleep 2
}

park "$VET" curl 'https://plain-get.example/v1/things?q=1&page=2'

park "$VET" curl -X POST -H 'X-Request-Id: abc123' -u alice:hunter2 \
    -d '{"name":"widget","qty":3}' https://headers-body.example/submit

park "$VET" curl -o "$MISSING" https://writes-file.example/blob

park "$VET" curl -k -T "$FIX/upload.bin" https://risky.example:9001/upload

park "$VET" --dry-run curl https://dry-run.example/check

park "$VET" curl http://localhost:3000/health

# Argv-derived text must never reach Pango markup or the notification
# body unescaped; this card is the fixture that proves it renders as
# literal text rather than restyling the surface it is displayed on.
park "$VET" curl -H "X-Evil: <span foreground='#00ff00'>pwned</span>" \
    "https://markup.example/<span foreground='#ff0000'>x</span>"

"$VET" daemon list
