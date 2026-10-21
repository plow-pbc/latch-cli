#!/bin/bash
# Measures the app against its budget and checks the menu-bar paths, on an
# isolated state folder under the repo's .dev/. Builds the test app (test hooks,
# bundle id co.plow.latch.test), so nothing here reads or removes the real
# app's preferences or saved windows.
#   scripts/measure.sh [--latch PATH] [OUT_DIR]
# --latch PATH  the latch the app runs (default: scripts/stub-latch, until the
#               CLI has `status --local` and `events --follow`)
# Budget: warm launch median <= 250 ms and worst <= 400 ms (10 runs after one
# discarded); idle 60 s with every window closed: CPU <= 0.1 %, idle wakeups
# <= 6, footprint <= 50 MB; menu bar state follows the host within 2 s.
set -eu
cd "$(dirname "$0")/.."
latch="$PWD/scripts/stub-latch"
if [ "${1:-}" = "--latch" ]; then latch="$2"; shift 2; fi
out="${1:-../../.dev/evidence/mac-app/chunk-2}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
state="$(cd ../.. && pwd)/.dev/mac-app/measure-state"
rm -rf "$state" && mkdir -p "$state"

bundle="$PWD/$(./scripts/build-app.sh --test-hooks --latch "$latch" | tail -n 1)"
test_id="co.plow.latch.test"
[ "$(defaults read "$bundle/Contents/Info" CFBundleIdentifier)" = "$test_id" ] || { echo "not the test app: $bundle" >&2; exit 1; }
app="$bundle/Contents/MacOS/PlowLatch"
tool="$(swift build --show-bin-path)/../measure-tool"
mkdir -p "$(dirname "$tool")"
swiftc -O -o "$tool" scripts/measure.swift

emit() { "$latch" --state-dir "$state" emit "$@"; }
args=(-LatchExecutable "$bundle/Contents/Helpers/latch" -LatchStateDir "$state"
      -ApplePersistenceIgnoreState YES -Onboarded YES)
pid=""
post() { "$tool" post "$@"; }
report() { rm -f "$state/report.json"; post report "$state/report.json"; sleep 0.5; cat "$state/report.json"; echo; }
quit_app() {
    [ -n "$pid" ] || return 0
    post quit
    for _ in $(seq 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    kill "$pid" 2>/dev/null || true
    pid=""
}
descendants() { local p; for p in $(pgrep -P "$1"); do echo "$p"; descendants "$p"; done; }
cputime() { ps -o cputime= -p "$1" | awk -F'[:.]' '{ print ($1 * 60 + $2) + $3 / 100 }'; }
idlew() { top -l 1 -pid "$1" -stats idlew | tail -n 1 | tr -d ' +'; }
footprint_mb() {
    footprint "$1" 2>/dev/null | awk '/Footprint:/ { for (i = 1; i < NF; i++) if ($i == "Footprint:") { v = $(i + 1); u = $(i + 2) }
        if (u == "KB") v /= 1024; if (u == "GB") v *= 1024; print v + 0; exit }'
}
now_ms() { perl -MTime::HiRes=time -e 'printf "%d", time * 1000'; }
sum() { awk '{ s += $1 } END { print s + 0 }'; }
# The test app's preferences and saved windows: none may outlive a run.
cleanup() {
    quit_app
    defaults delete "$test_id" 2>/dev/null || true
    rm -f "$HOME/Library/Preferences/$test_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$test_id.savedState"
}
trap cleanup EXIT

# The stub can be told what the host is doing; a real latch's host is
# followed in scripts/real-cli-check.py, with a real serve.
stub=false; [ "$latch" = "$PWD/scripts/stub-latch" ] && stub=true
if $stub; then emit host serving; fi

echo "== warm launch (1 discarded + 10)" | tee "$out/launch.txt"
times=()
for i in $(seq 0 10); do
    read -r ms pid < <("$tool" launch-ms "$app" "${args[@]}")
    quit_app
    echo "run $i: $ms ms$([ "$i" = 0 ] && echo ' (discarded)')" | tee -a "$out/launch.txt"
    [ "$i" = 0 ] || times+=("$ms")
    sleep 0.5
done
printf '%s\n' "${times[@]}" | sort -n | awk '{ a[NR] = $1 } END {
    median = (a[5] + a[6]) / 2
    printf "median %.1f ms (budget 250), worst %.1f ms (budget 400): %s\n", median, a[NR],
        (median <= 250 && a[NR] <= 400) ? "PASS" : "FAIL" }' | tee -a "$out/launch.txt"

echo "== idle: 60 s, every window closed, status subscription running" | tee "$out/idle.txt"
read -r _ pid < <("$tool" launch-ms "$app" "${args[@]}")
post close-windows
sleep 5
report | tee -a "$out/idle.txt"
kids=$(descendants "$pid" | tr '\n' ' ')
echo "app pid $pid; latch child processes: $kids" | tee -a "$out/idle.txt"
for k in $kids; do ps -o pid=,command= -p "$k"; done | tee -a "$out/idle.txt"
cpu0=$(cputime "$pid"); w0=$(idlew "$pid")
kcpu0=$(for k in $kids; do cputime "$k"; done | sum)
sleep 60
cpu1=$(cputime "$pid"); w1=$(idlew "$pid")
kcpu1=$(for k in $kids; do cputime "$k"; done | sum)
fp=$(footprint_mb "$pid")
kfp=$(for k in $kids; do footprint_mb "$k"; done | sum)
awk -v c0="$cpu0" -v c1="$cpu1" -v w0="$w0" -v w1="$w1" -v fp="$fp" -v k0="$kcpu0" -v k1="$kcpu1" -v kfp="$kfp" 'BEGIN {
    cpu = (c1 - c0) / 60 * 100; wakes = w1 - w0
    printf "app: CPU %.3f %% (budget 0.1), idle wakeups %d (budget 6), footprint %.1f MB (budget 50): %s\n",
        cpu, wakes, fp, (cpu <= 0.1 && wakes <= 6 && fp <= 50) ? "PASS" : "FAIL"
    printf "latch children (separate): CPU %.3f %%, footprint %.1f MB\n", (k1 - k0) / 60 * 100, kfp }' | tee -a "$out/idle.txt"

if $stub; then
echo "== menu bar follows the host, every window closed" | tee "$out/menu-latency.txt"
for target in stopped serving; do
    want="link.circle"; [ "$target" = serving ] && want="link.circle.fill"
    start=$(now_ms)
    emit host "$target"
    result="no change within 5 s: FAIL"
    while [ $(($(now_ms) - start)) -lt 5000 ]; do
        rm -f "$state/report.json"; post report "$state/report.json"; sleep 0.05
        if grep -q "\"menu\":\"$want\"" "$state/report.json" 2>/dev/null; then
            ms=$(($(now_ms) - start))
            result="menu icon $want after $ms ms (budget 2000): $([ "$ms" -le 2000 ] && echo PASS || echo FAIL)"
            break
        fi
    done
    echo "$target: $result" | tee -a "$out/menu-latency.txt"
done
fi
quit_app

echo "== open from the menu bar: Dock mode, then menu-bar-only mode" | tee "$out/open-from-menu.txt"
for keep in NO YES; do
    read -r _ pid < <("$tool" launch-ms "$app" "${args[@]}" -KeepInMenuBar "$keep")
    echo "-- KeepInMenuBar $keep" | tee -a "$out/open-from-menu.txt"
    post close-windows; sleep 1
    echo -n "all windows closed: " | tee -a "$out/open-from-menu.txt"; report | tee -a "$out/open-from-menu.txt"
    post show-main; sleep 1
    echo -n "Open Plow Latch…:   " | tee -a "$out/open-from-menu.txt"; report | tee -a "$out/open-from-menu.txt"
    post close-windows; sleep 1
    post show-settings; sleep 1
    echo -n "Settings…:          " | tee -a "$out/open-from-menu.txt"; report | tee -a "$out/open-from-menu.txt"
    quit_app
    sleep 0.5
done
echo "leftover latch processes: $(pgrep -f "$state" | tr '\n' ' ' || true)" | tee -a "$out/open-from-menu.txt"
