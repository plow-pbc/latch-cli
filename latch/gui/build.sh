#!/bin/sh
# Builds PlowLatchUI, the Mac app's window, as one universal library in
# gui/dist/ (Apple silicon and Intel), and prints its path. It needs Xcode 26
# or later (the macOS 26 SDK). Signing is scripts/macos-sign-app.sh's, with
# the rest of the app.
set -eu
cd "$(dirname "$0")"
swift build -c release --arch arm64 --arch x86_64 --product PlowLatchUI
mkdir -p dist
cp "$(swift build -c release --arch arm64 --arch x86_64 --show-bin-path)/libPlowLatchUI.dylib" dist/
echo "$PWD/dist/libPlowLatchUI.dylib"
