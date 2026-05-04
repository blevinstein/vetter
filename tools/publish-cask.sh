#!/usr/bin/env bash
# tools/publish-cask.sh — publish the macOS distribution artefact to
# the GitHub Release and bump the Homebrew tap.
#
# Runs immediately after tools/release.sh has produced
# target/Vetter-<VERSION>.zip on the same workstation, and after the
# matching git tag has been pushed (see "Cutting a release" in
# plans/Release.md).
#
# Inputs (env vars):
#   TAP_DIR        path to a clone of blevinstein/homebrew-vetter
#                  (default: $HOME/dev/homebrew-vetter)
#   RELEASE_REPO   GitHub repo to publish the Release on
#                  (default: blevinstein/vetter)
#
# Side effects, in order:
#   1. `gh release create v$VERSION` on $RELEASE_REPO with
#      target/Vetter-$VERSION.zip attached. Idempotent: if the
#      Release already exists, the asset is re-uploaded with
#      --clobber so re-running after a botched publish is safe.
#   2. Bumps the `version` + `sha256` stanzas in
#      $TAP_DIR/Casks/vetter.rb in place.
#   3. Commits + pushes the cask bump to `main` on the tap remote.
#
# Exits 78 (config error) if prereqs are missing; 0 on success.
#
# This is the same body the future CI release-on-tag workflow will
# run; only the auth surface differs (`gh auth login` locally →
# GITHUB_TOKEN in CI; ssh tap remote → an
# https://x-access-token:$TOKEN@… remote backed by a deploy key).

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

TAP_DIR="${TAP_DIR:-$HOME/dev/homebrew-vetter}"
RELEASE_REPO="${RELEASE_REPO:-blevinstein/vetter}"

if ! command -v gh >/dev/null 2>&1; then
    echo "publish-cask.sh: gh CLI not found. Install with 'brew install gh'." >&2
    exit 78
fi

if ! gh auth status >/dev/null 2>&1; then
    echo "publish-cask.sh: gh is not authenticated. Run 'gh auth login'." >&2
    exit 78
fi

if [[ ! -f "$TAP_DIR/Casks/vetter.rb" ]]; then
    {
        echo "publish-cask.sh: tap clone not found at $TAP_DIR."
        echo "  Clone with:"
        echo "    git clone git@github.com:blevinstein/homebrew-vetter $TAP_DIR"
        echo "  Or set TAP_DIR=/path/to/homebrew-vetter."
    } >&2
    exit 78
fi

# Re-derive VERSION + SHA256 from the workspace + zip on disk so this
# script is independent of tools/release.sh's stdout and can be
# re-run if any of the steps below need redoing.
# shellcheck source=tools/_bundle_layout.sh
source "$ROOT/tools/_bundle_layout.sh"
VERSION=$(vetter_workspace_version)
ZIP="target/Vetter-$VERSION.zip"

if [[ ! -f "$ZIP" ]]; then
    echo "publish-cask.sh: $ZIP not found. Run tools/release.sh first." >&2
    exit 78
fi
SHA256=$(shasum -a 256 "$ZIP" | awk '{print $1}')

# The git tag drives the Release page URL and the cask `url`. If it
# isn't on origin yet, gh would silently create the tag at the
# default branch HEAD — refuse instead so the operator pushes the
# intended commit explicitly.
if ! git ls-remote --tags origin "v$VERSION" 2>/dev/null \
        | grep -q "refs/tags/v$VERSION$"; then
    {
        echo "publish-cask.sh: git tag v$VERSION not on origin."
        echo "  Push it first:"
        echo "    git tag v$VERSION && git push origin v$VERSION"
    } >&2
    exit 78
fi

echo "publish-cask.sh: publishing v$VERSION to $RELEASE_REPO and $TAP_DIR ..."
echo "  zip:    $ZIP"
echo "  sha256: $SHA256"

# 1. Publish the GitHub Release. Do this before the cask bump so the
#    cask `url` resolves the moment it lands on the tap. Idempotent
#    via the view-then-create-or-upload split.
if gh release view "v$VERSION" --repo "$RELEASE_REPO" >/dev/null 2>&1; then
    echo "publish-cask.sh: Release v$VERSION already exists; re-uploading asset."
    gh release upload "v$VERSION" --repo "$RELEASE_REPO" --clobber "$ZIP"
else
    gh release create "v$VERSION" \
        --repo "$RELEASE_REPO" \
        --title "v$VERSION" \
        --generate-notes \
        "$ZIP"
fi

# 2. Bump the cask in place. The two-line sed matches the version +
#    sha256 stanzas regardless of their previous values, so it's safe
#    to re-run after a botched publish (it's a no-op if already
#    correct, which step 3 detects and skips committing).
sed -i.bak -E \
    -e "s/^(  version )\".*\"$/\1\"$VERSION\"/" \
    -e "s/^(  sha256 )\".*\"$/\1\"$SHA256\"/" \
    "$TAP_DIR/Casks/vetter.rb"
rm "$TAP_DIR/Casks/vetter.rb.bak"

# 3. Commit + push the cask bump on main. Skip the commit if the
#    cask already matches (re-run after a successful publish, or a
#    hand-edit that already set the right values), but still push so
#    any local-only commits on the tap propagate.
if git -C "$TAP_DIR" diff --quiet -- Casks/vetter.rb; then
    echo "publish-cask.sh: cask already at v$VERSION; nothing to commit."
else
    git -C "$TAP_DIR" commit -m "vetter v$VERSION" -- Casks/vetter.rb
fi
git -C "$TAP_DIR" push origin main

cat <<EOF

publish-cask.sh: done.
  release: https://github.com/$RELEASE_REPO/releases/tag/v$VERSION
  cask:    $TAP_DIR/Casks/vetter.rb (version $VERSION, sha256 $SHA256)

End users can now:
  brew update && brew upgrade --cask vetter
EOF
