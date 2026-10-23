#!/bin/sh
# macOS: this checkout's debug latch, put in its app (macos/identity.sh) with
# the window built for Apple silicon and Intel as a release does, signs ad
# hoc, verifies, and runs from inside the bundle. Run from latch/, where cargo
# resolves a relative CARGO_TARGET_DIR.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
. "$root/latch/macos/identity.sh"
app="$tmp/$LATCH_APP_NAME.app"
"$root/latch/gui/build.sh" >/dev/null
sh "$root/scripts/macos-app-bundle.sh" 0.0.0 "${CARGO_TARGET_DIR:-target}/debug/latch" "$app"
sh "$root/scripts/macos-sign-app.sh" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
for arch in arm64 x86_64; do
  lipo "$app/Contents/Frameworks/libPlowLatchUI.dylib" -verify_arch "$arch"
done
"$app/Contents/MacOS/latch" --version
