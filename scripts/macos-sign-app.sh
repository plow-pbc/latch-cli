#!/bin/sh
# Signs Plow Latch.app from the inside out: the window's library in
# Contents/Frameworks first, then the app, whose executable is latch. With no
# identity, ad hoc (a build that keeps its secrets in files); with the
# Developer ID identity and its keychain, as a release does: the hardened
# runtime, a time stamp, and latch/macos/latch.entitlements on the app.
#   macos-sign-app.sh BUNDLE [IDENTITY KEYCHAIN]
set -eu
[ "$#" -eq 1 ] || [ "$#" -eq 3 ] || { echo 'usage: macos-sign-app.sh BUNDLE [IDENTITY KEYCHAIN]' >&2; exit 1; }
bundle=$1
here=$(cd "$(dirname "$0")/.." && pwd)
if [ "$#" -eq 1 ]; then
  codesign --force --sign - "$bundle"/Contents/Frameworks/*.dylib
  codesign --force --sign - "$bundle"
else
  identity=$2 keychain=$3
  codesign --force --options runtime --timestamp --keychain "$keychain" --sign "$identity" \
    "$bundle"/Contents/Frameworks/*.dylib
  codesign --force --options runtime --timestamp --keychain "$keychain" \
    --entitlements "$here/latch/macos/latch.entitlements" --sign "$identity" "$bundle"
fi
