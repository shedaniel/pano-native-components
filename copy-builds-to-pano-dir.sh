#!/bin/bash
# Build the release dylibs/so/dll and copy them into the pano-scrobbler
# resources dir for the current OS + arch (local dev loop).
set -euo pipefail

cargo build --release

PANO_DIR="../pano-scrobbler"
os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin)
    ext="dylib"; prefix="lib"
    if [ "$arch" = "arm64" ]; then resourcesDirName="macos-arm64"; else resourcesDirName="macos-x64"; fi
    ;;
  Linux)
    ext="so"; prefix="lib"
    if [ "$arch" = "aarch64" ]; then resourcesDirName="linux-arm64"; else resourcesDirName="linux-x64"; fi
    ;;
  *) # Windows (Git Bash / MSYS)
    ext="dll"; prefix=""
    resourcesDirName="windows-x64"
    ;;
esac

destDir="$PANO_DIR/composeApp/resources/$resourcesDirName"
mkdir -p "$destDir"
cp "target/release/${prefix}pano_native_components.${ext}" "$destDir/"

# native_webview is Windows + Linux only (macOS uses OOB browser login; no
# WKWebView port). Skip it on Darwin — the dylib isn't built meaningfully there.
if [ "$os" != "Darwin" ]; then
    cp "target/release/${prefix}native_webview.${ext}" "$destDir/"
    echo "Copied ${prefix}pano_native_components.${ext} and ${prefix}native_webview.${ext} -> $destDir"
else
    echo "Copied ${prefix}pano_native_components.${ext} -> $destDir (native_webview not built on macOS)"
fi

# On macOS, also build + place the third-party MediaRemote adapter so the app
# can detect now-playing. One build tool for both dylibs and the adapter.
if [ "$os" = "Darwin" ]; then
    bash "$(dirname "$0")/build-macos-adapter.sh" "$destDir/mediaremote-adapter"
fi
