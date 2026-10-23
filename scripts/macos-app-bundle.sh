#!/bin/sh
# Wraps a macOS latch binary in Plow Latch.app: Contents/MacOS/latch, its
# executable; the app's window, latch/gui's library (gui/build.sh), in
# Contents/Frameworks, which latch loads when the app is opened (src/gui.rs);
# the icon; the bundle id (latch/macos/identity.sh) that Full Disk Access and
# the Developer ID signature name; and the Developer ID provisioning profile
# that lets a signature claim latch's keychain group; and the launch agent
# the app registers for "Start when you log in", which runs its latch's
# `serve -d --again` at login (Contents/Library/LaunchAgents/ID.login.plist,
# registered with ServiceManagement). --id and --name make
# another app instead (a dev build's, so it never shares the installed app's
# grants, preferences or windows). Signing is scripts/macos-sign-app.sh's.
#   macos-app-bundle.sh [--id ID --name NAME] VERSION BINARY BUNDLE
set -eu
usage='usage: macos-app-bundle.sh [--id ID --name NAME] VERSION BINARY BUNDLE'
here=$(cd "$(dirname "$0")/.." && pwd)
. "$here/latch/macos/identity.sh"
while [ $# -gt 3 ]; do
  case "$1" in
    --id) LATCH_BUNDLE_ID=$2; shift 2 ;;
    --name) LATCH_APP_NAME=$2; shift 2 ;;
    *) echo "$usage" >&2; exit 1 ;;
  esac
done
[ "$#" -eq 3 ] || { echo "$usage" >&2; exit 1; }
version=$1 binary=$2 bundle=$3
gui="$here/latch/gui/dist/libPlowLatchUI.dylib"
[ -f "$gui" ] || { echo "$gui is missing: build it with latch/gui/build.sh" >&2; exit 1; }
# The version without its tag, as a person reads it.
short=${version#v}
short=${short%%-*}
# What macOS and Sparkle compare builds by: a release's build number (the
# tag's v0.1.0-1327), so each build is distinct; else the version.
build=$short
case "$version" in
  *-*) case "${version##*-}" in '' | *[!0-9]*) ;; *) build=${version##*-} ;; esac ;;
esac
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Frameworks" "$bundle/Contents/Resources" \
  "$bundle/Contents/Library/LaunchAgents"
cp "$binary" "$bundle/Contents/MacOS/latch"
chmod 755 "$bundle/Contents/MacOS/latch"
cp "$gui" "$bundle/Contents/Frameworks/"
cp "$here/latch/macos/AppIcon.icns" "$bundle/Contents/Resources/AppIcon.icns"
cp "$here/latch/macos/PlowLatchCLI-DeveloperID.provisionprofile" "$bundle/Contents/embedded.provisionprofile"
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>$LATCH_BUNDLE_ID</string>
	<key>CFBundleName</key>
	<string>$LATCH_APP_NAME</string>
	<key>CFBundleDisplayName</key>
	<string>$LATCH_APP_NAME</string>
	<key>CFBundleExecutable</key>
	<string>latch</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$short</string>
	<key>CFBundleVersion</key>
	<string>$build</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>NSLocalNetworkUsageDescription</key>
	<string>Latch uses your local network so your agents can connect to this Mac directly and, when your rules let them, reach devices on your network. The connection between your agents and this Mac is encrypted end to end.</string>
</dict>
</plist>
PLIST
cat > "$bundle/Contents/Library/LaunchAgents/$LATCH_BUNDLE_ID.login.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$LATCH_BUNDLE_ID.login</string>
	<key>BundleProgram</key>
	<string>Contents/MacOS/latch</string>
	<key>ProgramArguments</key>
	<array>
		<string>latch</string>
		<string>serve</string>
		<string>-d</string>
		<string>--again</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>$LATCH_BUNDLE_ID</string>
	</array>
</dict>
</plist>
PLIST
