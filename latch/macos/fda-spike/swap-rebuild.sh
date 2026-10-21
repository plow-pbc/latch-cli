#!/bin/bash
# Puts build `two` (same identity, new cdhash) in place of build `one`, at
# the same paths, without touching System Settings.  swap-rebuild.sh [RUN]
set -euo pipefail
run=${1:-$(cd "$(dirname "$0")/../../.." && pwd)/run}
for part in A B C; do
  # Removed, not overwritten: a new file, as an update installs one.
  rm -rf "${run:?}/$part"
  cp -R "$run/rebuild/$part" "$run/$part"
done
for item in "$run/A/fda-probe" "$run/B/Plow Latch CLI.app" "$run/C/latch"; do
  codesign -dvvv "$item" 2>&1 | grep -E '^CDHash=' || true
done
echo "build two in place"
