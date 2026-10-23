#!/bin/sh
# Builds latch-messages, the Messages reader a macOS host runs, as one
# universal binary in messages-reader/dist/, signs it (ad hoc unless
# LATCH_CODESIGN_IDENTITY names a Developer ID), and prints its sha256: the
# pin a host checks it against.
set -eu
cd "$(dirname "$0")"
. ../macos/identity.sh
swift build -c release --arch arm64 --arch x86_64 --product latch-messages
mkdir -p dist
cp "$(swift build -c release --arch arm64 --arch x86_64 --show-bin-path)/latch-messages" dist/latch-messages
# A Developer ID signature is time-stamped, as Gatekeeper needs; ad hoc has none.
identity=${LATCH_CODESIGN_IDENTITY:--}
timestamp=--timestamp
[ "$identity" != - ] || timestamp=--timestamp=none
codesign --force --options runtime "$timestamp" --identifier "$LATCH_READER_ID" --sign "$identity" dist/latch-messages
shasum -a 256 dist/latch-messages | cut -d ' ' -f 1
