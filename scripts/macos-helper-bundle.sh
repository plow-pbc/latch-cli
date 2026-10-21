#!/bin/sh
# Wrap a latch binary in the app-like helper bundle macOS needs before it
# honours a provisioning profile: Latch.app/Contents/{Info.plist,
# MacOS/latch, embedded.provisionprofile}. The profile is embedded when
# latch/macos has one. Signing is the caller's.
set -eu
[ "$#" -eq 2 ] || { echo 'usage: macos-helper-bundle.sh BINARY BUNDLE' >&2; exit 1; }
binary=$1 bundle=$2
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' latch/Cargo.toml | head -1)
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS"
cp "$binary" "$bundle/Contents/MacOS/latch"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>co.plow.latch.cli</string>
	<key>CFBundleName</key>
	<string>latch</string>
	<key>CFBundleExecutable</key>
	<string>latch</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSBackgroundOnly</key>
	<true/>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
</dict>
</plist>
PLIST
for profile in latch/macos/*.provisionprofile; do
  if [ -f "$profile" ]; then cp "$profile" "$bundle/Contents/embedded.provisionprofile"; fi
done
