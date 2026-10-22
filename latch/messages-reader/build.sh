#!/bin/sh
# Builds latch-messages, the Messages reader a macOS host runs, as one
# universal binary in messages-reader/dist/, signs it (ad hoc unless
# LATCH_CODESIGN_IDENTITY names a Developer ID), and prints its sha256: the
# pin a host checks it against.
set -eu
cd "$(dirname "$0")"
swift build -c release --arch arm64 --arch x86_64 --product latch-messages
mkdir -p dist
cp "$(swift build -c release --arch arm64 --arch x86_64 --show-bin-path)/latch-messages" dist/latch-messages
codesign --force --options runtime --identifier co.plow.latch.messages --sign "${LATCH_CODESIGN_IDENTITY:--}" dist/latch-messages
shasum -a 256 dist/latch-messages | cut -d ' ' -f 1
