#!/bin/sh
# macOS: this checkout's debug latch, put in Plow Latch.app as a release
# does, signs ad hoc, verifies, and runs from inside the bundle. Run from
# latch/, where cargo resolves a relative CARGO_TARGET_DIR.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
app="$tmp/Plow Latch.app"
sh "$root/scripts/macos-app-bundle.sh" 0.0.0 "${CARGO_TARGET_DIR:-target}/debug/latch" "$app"
codesign --force --sign - "$app"
codesign --verify --deep --strict --verbose=2 "$app"
"$app/Contents/MacOS/latch" --version
