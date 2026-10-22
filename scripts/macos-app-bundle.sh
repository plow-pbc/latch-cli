#!/bin/sh
# Wraps a macOS latch binary in Plow Latch.app: no window or Dock icon,
# Contents/MacOS/latch, the icon, the bundle id co.plow.latch.cli that
# Full Disk Access and the Developer ID signature name, and the Developer ID
# provisioning profile that lets a signature claim latch's keychain group.
# Signing is the caller's: ad hoc for a build that keeps files, Developer ID
# with latch/macos/latch.entitlements for one that keeps the Keychain.
#   macos-app-bundle.sh VERSION BINARY BUNDLE
set -eu
[ "$#" -eq 3 ] || { echo 'usage: macos-app-bundle.sh VERSION BINARY BUNDLE' >&2; exit 1; }
version=$1 binary=$2 bundle=$3
here=$(cd "$(dirname "$0")/.." && pwd)
# CFBundleVersion takes digits and dots only: the version without its tag.
short=${version#v}
short=${short%%-*}
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$binary" "$bundle/Contents/MacOS/latch"
chmod 755 "$bundle/Contents/MacOS/latch"
cp "$here/latch/macos/AppIcon.icns" "$bundle/Contents/Resources/AppIcon.icns"
cp "$here/latch/macos/PlowLatchCLI-DeveloperID.provisionprofile" "$bundle/Contents/embedded.provisionprofile"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>co.plow.latch.cli</string>
	<key>CFBundleName</key>
	<string>Plow Latch</string>
	<key>CFBundleDisplayName</key>
	<string>Plow Latch</string>
	<key>CFBundleExecutable</key>
	<string>latch</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$short</string>
	<key>CFBundleVersion</key>
	<string>$short</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
</dict>
</plist>
PLIST
