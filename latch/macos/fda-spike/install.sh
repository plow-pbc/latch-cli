#!/bin/bash
# Downloads the signed probes from a fda-spike release run and puts build
# `one` in place under RUN (default: the worktree's run/), build `two`
# beside it for the rebuild test.  install.sh RUN_ID [RUN]
set -euo pipefail
run_id=$1
run=${2:-$(cd "$(dirname "$0")/../../.." && pwd)/run}
mkdir -p "$run"
tmp=$(mktemp -d)
gh run download "$run_id" -R plow-pbc/latch-cli -n fda-spike -D "$tmp"
tar -xzf "$tmp/fda-spike.tar.gz" -C "$tmp"
rm -rf "$run/A" "$run/B" "$run/C" "$run/rebuild"
cp -R "$tmp/spike/one/A" "$tmp/spike/one/B" "$tmp/spike/one/C" "$run/"
mkdir -p "$run/rebuild"
cp -R "$tmp/spike/two/A" "$tmp/spike/two/B" "$tmp/spike/two/C" "$run/rebuild/"
rm -rf "$tmp"
for item in "$run/A/fda-probe" "$run/B/Plow Latch CLI.app" "$run/C/latch"; do
  codesign --verify --strict "$item" && { codesign -dvvv "$item" 2>&1 | grep -E '^(Identifier|TeamIdentifier|CDHash)=' || true; }
  xattr -l "$item" | grep -q quarantine && echo "note: $item is quarantined" || true
done
echo "installed build one in $run"
