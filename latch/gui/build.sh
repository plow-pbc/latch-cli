#!/bin/sh
# Builds PlowLatchUI, the Mac app's window, as one universal library in
# gui/dist/ (Apple silicon and Intel), and prints its path. It needs Xcode 26
# or later (the macOS 26 SDK). Signing is scripts/macos-sign-app.sh's, with
# the rest of the app. --test-hooks builds a test app's instead, which an
# end-to-end run drives with distributed notifications (App/TestHooks.swift).
#   build.sh [--test-hooks]
set -eu
cd "$(dirname "$0")"
flags=""
[ "${1:-}" = --test-hooks ] && flags="-Xswiftc -DLATCH_TEST_HOOKS"
# shellcheck disable=SC2086
swift build -c release --arch arm64 --arch x86_64 --product PlowLatchUI $flags
mkdir -p dist
# shellcheck disable=SC2086
cp "$(swift build -c release --arch arm64 --arch x86_64 --show-bin-path $flags)/libPlowLatchUI.dylib" dist/
echo "$PWD/dist/libPlowLatchUI.dylib"
