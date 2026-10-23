#!/bin/sh
# Assemble a binary (on macOS, Plow Latch.app) and the files
# install-binary.sh reads, without browser downloads.
set -eu
[ "$#" -eq 3 ] || { echo 'usage: package-release.sh VERSION TARGET BINARY' >&2; exit 1; }
version=$1 target=$2 binary=$3
# The test-only gog stand-in never ships: its marker is in no binary.
if grep -aqF latch-fake-gog-test-only "$binary"; then
  echo "$binary was built with the fake-gog test feature" >&2
  exit 1
fi
cd "$(dirname "$0")/.."
out="dist/latch-$version-$target"
mkdir -p "$out/src/google"
case "$target" in
  # macOS ships Plow Latch.app, ad-hoc signed until the release signs it;
  # `latch` beside install-binary.sh leads into it.
  *-apple-darwin)
    . latch/macos/identity.sh
    sh scripts/macos-app-bundle.sh "$version" "$binary" "$out/$LATCH_APP_NAME.app"
    sh scripts/macos-sign-app.sh "$out/$LATCH_APP_NAME.app"
    ln -sf "$LATCH_APP_NAME.app/Contents/MacOS/latch" "$out/latch"
    ;;
  *) cp "$binary" "$out/latch" ;;
esac
cp latch/install-binary.sh latch/install-packages.sh "$out/"
cp latch/src/google/pin.json "$out/src/google/"
if [ -d latch/src/browser ]; then
  mkdir -p "$out/src/browser" "$out/camoufox"
  cp latch/browser-engines.sh "$out/"
  cp latch/src/browser/cft.json latch/src/browser/camoufox.json "$out/src/browser/"
  cp latch/camoufox/package.json latch/camoufox/package-lock.json "$out/camoufox/"
  cp -R latch/camoufox/sqlite "$out/camoufox/"
fi
# The Messages reader, built and pinned with a macOS latch (package.yml).
if [ -f latch/messages-reader/dist/latch-messages ]; then
  cp latch/messages-reader/dist/latch-messages "$out/"
fi
cp latch/DOCKER.md "$out/"
mkdir -p "$out/docker"
cp latch/docker/seccomp.json latch/docker/latch-bwrap.apparmor "$out/docker/"
[ -L "$out/latch" ] || chmod 755 "$out/latch"
tar -czf "$out.tar.gz" -C dist "latch-$version-$target"
