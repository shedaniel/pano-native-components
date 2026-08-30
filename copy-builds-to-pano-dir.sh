#!/bin/bash

cargo build --release
if [ "$(uname -m)" = "aarch64" ]; then
    resourcesDirName="linux-arm64"
else
    resourcesDirName="linux-x64"
fi
cp target/release/libpano_native_components.so ../pano-scrobbler/desktopApp/resources/$resourcesDirName/;
cp target/release/libnative_webview.so ../pano-scrobbler/desktopApp/resources/$resourcesDirName/;
