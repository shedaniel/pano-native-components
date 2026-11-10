#!/usr/bin/env bash
# Build the third-party MediaRemote adapter (ungive/mediaremote-adapter) and
# copy its two artifacts — the perl wrapper script and the helper framework —
# into a destination directory.
#
# This is macOS-only: the framework bridges to the private MediaRemote API
# via the Apple-signed system perl, so it must be built and used on macOS.
#
# Usage:
#   ./build-macos-adapter.sh [DEST_DIR]
#
# DEST_DIR defaults to
#   ../pano-scrobbler/composeApp/resources/macos-arm64/mediaremote-adapter
#
# Prereqs: git, cmake, clang, Xcode command line tools, /usr/bin/perl.
# Safe to re-run; rebuilds in place.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENDOR="$HERE/vendor"
REPO="$VENDOR/mediaremote-adapter"
REPO_URL="https://github.com/ungive/mediaremote-adapter.git"
DEST="${1:-$HERE/../pano-scrobbler/composeApp/resources/macos-arm64/mediaremote-adapter}"

die() { printf '\033[1;31m  ✗ %s\033[0m\n' "$*" >&2; exit 1; }

# ── Preconditions ────────────────────────────────────────────────────────────
[ "$(uname -s)" = "Darwin" ] || die "This script only runs on macOS."
[ -x /usr/bin/perl ]          || die "/usr/bin/perl not found (needed for the adapter)."
command -v git >/dev/null      || die "git not found"
command -v cmake >/dev/null    || die "cmake not found (try: brew install cmake)"
command -v clang >/dev/null    || die "clang not found (install Xcode / command line tools)"

# ── Clone / update ───────────────────────────────────────────────────────────
mkdir -p "$VENDOR"
if [ -d "$REPO/.git" ]; then
    git -C "$REPO" pull --ff-only || true
else
    git clone --depth 1 "$REPO_URL" "$REPO"
fi

# ── Build ────────────────────────────────────────────────────────────────────
BUILD="$REPO/build"
mkdir -p "$BUILD"
cmake -S "$REPO" -B "$BUILD" >/dev/null
cmake --build "$BUILD" >/dev/null

# ── Resolve + copy artifacts ─────────────────────────────────────────────────
FRAMEWORK="$(/usr/bin/find "$BUILD" -maxdepth 3 -name 'MediaRemoteAdapter.framework' -type d | head -n1)"
SCRIPT="$REPO/bin/mediaremote-adapter.pl"
[ -d "$FRAMEWORK" ] || die "MediaRemoteAdapter.framework not found under $BUILD after build"
[ -f "$SCRIPT" ]    || die "mediaremote-adapter.pl not found at $SCRIPT"

mkdir -p "$DEST"
rm -rf "$DEST/MediaRemoteAdapter.framework"
cp "$SCRIPT" "$DEST/mediaremote-adapter.pl"
cp -R "$FRAMEWORK" "$DEST/MediaRemoteAdapter.framework"

echo "MediaRemote adapter built and copied to $DEST"
