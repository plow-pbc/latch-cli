#!/bin/sh
# Assemble a binary and the files install.sh reads, without browser downloads.
set -eu
[ "$#" -eq 3 ] || { echo 'usage: package-release.sh VERSION TARGET BINARY' >&2; exit 1; }
version=$1 target=$2 binary=$3
cd "$(dirname "$0")/.."
out="dist/latch-$version-$target"
mkdir -p "$out/src/google" "$out/src/browser" "$out/camoufox"
cp "$binary" "$out/latch"
cp latch/install.sh "$out/"
cp latch/src/google/pin.json "$out/src/google/"
cp latch/src/browser/cft.json latch/src/browser/camoufox.json "$out/src/browser/"
cp latch/camoufox/package.json latch/camoufox/package-lock.json "$out/camoufox/"
cp -R latch/camoufox/sqlite "$out/camoufox/"
cp latch/DOCKER.md "$out/"
mkdir -p "$out/docker"
cp latch/docker/seccomp.json latch/docker/latch-bwrap.apparmor "$out/docker/"
chmod 755 "$out/latch"
tar -czf "$out.tar.gz" -C dist "latch-$version-$target"
