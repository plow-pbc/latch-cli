#!/bin/bash
# Runs one variant's probe from a throwaway launchd agent, waits for its
# output, unloads the agent and prints what it said.
#   run-variant.sh A|B|C|D [--dry] [RUN]
# A: the bare binary. B: the app bundle's executable. C: the bundle's
# executable starting the outside binary, which starts the probing worker.
# D: the control for C, the outside binary alone, with no bundle above it.
# --dry checks the plumbing without opening any protected path.
set -euo pipefail
variant=$1
dry=""
[[ "${2:-}" == --dry ]] && { dry=--dry; shift; }
run=${2:-$(cd "$(dirname "$0")/../../.." && pwd)/run}
app="$run/B/Plow Latch CLI.app/Contents/MacOS/latch"
case "$variant" in
  A) args=("$run/A/fda-probe") ;;
  B) args=("$app") ;;
  C) args=("$app" --chain "$run/C/latch") ;;
  D) args=("$run/C/latch") ;;
  *) echo "variant A, B, C or D" >&2; exit 2 ;;
esac
[[ -n "$dry" ]] && args+=("$dry")
label="co.plow.latch.fdaspike.$(tr A-Z a-z <<< "$variant")"
mkdir -p "$run/plists"
plist="$run/plists/$label.plist"
out="$run/out-$variant.txt"
rm -f "$out"
{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
  echo '<plist version="1.0"><dict>'
  echo "<key>Label</key><string>$label</string>"
  echo '<key>ProgramArguments</key><array>'
  for arg in "${args[@]}"; do echo "<string>$arg</string>"; done
  echo '</array><key>RunAtLoad</key><true/>'
  echo "<key>StandardOutPath</key><string>$out</string>"
  echo "<key>StandardErrorPath</key><string>$out</string>"
  echo '</dict></plist>'
} > "$plist"
domain="gui/$(id -u)"
launchctl bootout "$domain/$label" 2>/dev/null || true
launchctl bootstrap "$domain" "$plist"
for _ in $(seq 40); do
  grep -qE '^(addressbook_dir:|dry:)' "$out" 2>/dev/null && break
  sleep 0.5
done
launchctl print "$domain/$label" 2>/dev/null | grep -E 'last exit code' || true
launchctl bootout "$domain/$label" 2>/dev/null || true
echo "== variant $variant ($label)"
cat "$out" 2>/dev/null || echo "(no output)"
