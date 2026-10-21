#!/bin/bash
# Unloads every spike agent and removes everything the spike made: plists,
# probes and outputs. The Full Disk Access entries are the chef's to
# remove in System Settings.  cleanup.sh [RUN]
set -uo pipefail
run=${1:-$(cd "$(dirname "$0")/../../.." && pwd)/run}
for v in a b c d; do
  launchctl bootout "gui/$(id -u)/co.plow.latch.fdaspike.$v" 2>/dev/null && echo "unloaded co.plow.latch.fdaspike.$v"
done
rm -rf "${run:?}"
for v in a b c d; do
  launchctl print "gui/$(id -u)/co.plow.latch.fdaspike.$v" >/dev/null 2>&1 \
    && echo "STILL LOADED: co.plow.latch.fdaspike.$v" || echo "co.plow.latch.fdaspike.$v: not loaded"
done
echo "removed $run"
