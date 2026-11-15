#!/usr/bin/env bash
# Release OMCA for the two supported platforms: Apple silicon Macs and Windows x64.
#
#   packaging/release.sh [--skip-ci] [--no-windows]
#
# 1. Runs the local CI gate (`cargo xtask ci`) unless --skip-ci.
# 2. Builds the macOS aarch64 DMG, CLI ZIP, and signed update ZIP here.
#    OS signing follows MACOS_SIGN_IDENTITY / APPLE_*; updater signing requires
#    OMCA_UPDATE_SIGNING_KEY_FILE or OMCA_UPDATE_SIGNING_KEY.
# 3. Tags v<version>, pushes the tag, and publishes the exact macOS files and SHA-256 sums.
# 4. Starts the manual Windows x64 workflow, which attaches the MSI, portable ZIP, and their
#    signed update manifests to the same release, unless --no-windows.
set -euo pipefail
# shellcheck source=env.sh
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

RUN_CI=1
WINDOWS=1
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-ci) RUN_CI=0; shift ;;
    --no-windows) WINDOWS=0; shift ;;
    -h | --help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

TAG="v$VERSION"
cd "$ROOT"
# gh picks the upstream remote when no default is set; release to origin, never to upstream.
GH_REPO="$(git remote get-url origin | sed -E 's#\.git$##; s#^.*github\.com[:/]##')"
export GH_REPO
if [ -n "$(git status --porcelain)" ]; then
  echo "error: the working tree has uncommitted changes; commit them first" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "error: $TAG already exists; bump the version with: cargo xtask version set X.Y.Z" >&2
  exit 1
fi

if [ "$RUN_CI" = 1 ]; then
  cargo xtask ci
fi

"$ROOT/packaging/macos/package.sh"
macos_files=(
  "$DIST/craft-anything-$VERSION-macos-aarch64.dmg"
  "$DIST/craft-anything-cli-$VERSION-macos-aarch64.zip"
  "$DIST/omca-$VERSION-macos-aarch64-update.zip"
  "$DIST/omca-$VERSION-macos-aarch64-update.zip.manifest.json"
  "$DIST/omca-$VERSION-macos-aarch64-update.zip.sig"
)
for file in "${macos_files[@]}"; do
  test -s "$file" || { echo "error: missing release artifact: $file" >&2; exit 1; }
done
(cd "$DIST" && shasum -a 256 "${macos_files[@]##*/}" >SHA256SUMS.txt)

git tag -a "$TAG" -m "OMCA $VERSION"
# The gate ran above, or --skip-ci said it ran already: the pre-push hook would only run it again.
git push --no-verify origin "$TAG"
gh release create "$TAG" --title "OMCA $VERSION" --generate-notes \
  "${macos_files[@]}" "$DIST/SHA256SUMS.txt"

if [ "$WINDOWS" = 1 ]; then
  gh workflow run release-windows.yml -f tag="$TAG"
  echo "==> Windows x64 build started: gh run watch \$(gh run list -w release-windows.yml -L 1 --json databaseId -q '.[0].databaseId')"
fi
echo "==> released $TAG"
