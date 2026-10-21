#!/bin/bash
# Exercises background sharing end to end on an isolated state folder, with
# the test app's own agent label (co.plow.latch.test.serve), and makes sure
# nothing it registers outlives the run.
#   scripts/sharing-check.sh [--latch PATH] [--state DIR] [OUT_DIR]
# --latch PATH  the latch the agent runs (default: scripts/stub-latch)
# --state DIR   an isolated state folder already logged in (a real latch's
#               serve needs a login); default: a fresh one, for the stub
set -eu
cd "$(dirname "$0")/.."
latch="$PWD/scripts/stub-latch"
dev="$(cd ../.. && pwd)/.dev/mac-app"
state="$dev/sharing-state"
fresh=true
while [ $# -gt 0 ]; do
    case "$1" in
        --latch) latch="$2"; shift 2 ;;
        --state) state="$2"; fresh=false; shift 2 ;;
        *) break ;;
    esac
done
case "$state" in "$dev"/*) ;; *) echo "state must be under $dev" >&2; exit 2 ;; esac
out="${1:-../../.dev/evidence/mac-app/chunk-4}"
mkdir -p "$out"
out="$(cd "$out" && pwd)/sharing-check.txt"
if $fresh; then rm -rf "$state"; fi
mkdir -p "$state"
share="$dev/sharing-share"
mkdir -p "$share"

bundle="$PWD/$(./scripts/build-app.sh --test-hooks --latch "$latch" --serve-state-dir "$state" | tail -n 1)"
test_id="co.plow.latch.test"
label="$test_id.serve"
[ "$(defaults read "$bundle/Contents/Info" CFBundleIdentifier)" = "$test_id" ] || { echo "not the test app" >&2; exit 1; }
app="$bundle/Contents/MacOS/PlowLatch"
cli="$bundle/Contents/Helpers/latch"
tool="$(swift build --show-bin-path)/../measure-tool"
swiftc -O -o "$tool" scripts/measure.swift
args=(-LatchStateDir "$state" -ApplePersistenceIgnoreState YES -Onboarded YES)
domain="gui/$(id -u)"
pid=""
terminal_serve=""

say() { echo "$*" | tee -a "$out"; }
post() { "$tool" post "$@"; }
report() { rm -f "$state/report.json"; post report "$state/report.json"; sleep 0.5; cat "$state/report.json" 2>/dev/null; }
launch() { read -r _ pid < <("$tool" launch-ms "$app" "${args[@]}"); sleep 2; }
quit_app() {
    [ -n "$pid" ] || return 0
    post quit
    for _ in $(seq 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    pid=""
}
agent_line() {
    if launchctl print "$domain/$label" > "$state/launchctl.txt" 2>&1; then
        echo "launchd: $label loaded, $(grep -E '^\s*(state|pid) =' "$state/launchctl.txt" | tr -s ' \n' ' ')"
    else
        echo "launchd: $label not loaded"
    fi
}
serve_line() { echo "latch status: $("$cli" --state-dir "$state" status --local | jq -c .serve)"; }
wait_for() { # wait_for STATE_TITLE: until the app's report shows it, up to 30 s
    for _ in $(seq 60); do report | grep -q "\"state\":\"$1\"" && return 0; sleep 0.5; done
    return 1
}
# Whatever happens, leave nothing registered: the app unregisters its agent.
cleanup() {
    [ -n "$terminal_serve" ] && kill "$terminal_serve" 2>/dev/null || true
    if launchctl print "$domain/$label" > /dev/null 2>&1; then
        [ -n "$pid" ] || launch
        post stop-sharing; sleep 3
    fi
    quit_app
    say "== after the run"
    say "$(agent_line)"
    launch; say "the app's agent status: $(report | jq -r .agent)"; quit_app
    defaults delete "$test_id" 2>/dev/null || true
    rm -f "$HOME/Library/Preferences/$test_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$test_id.savedState"
}
trap cleanup EXIT

: > "$out"
say "== before the run"
say "$(agent_line)"
"$cli" --state-dir "$state" shares set "$share" > /dev/null

say "== the agent's plist: runs once per load, never restarted by launchd"
say "$(plutil -p "$bundle/Contents/Library/LaunchAgents/$label.plist")"

say "== 1. launch: sharing stopped, agent off"
launch; say "$(report)"; say "$(serve_line)"

say "== 2. Start Sharing: registered, and serving once latch says so"
post start-sharing
wait_for "Sharing" && say "$(report)" || say "FAIL: not serving: $(report)"
say "$(agent_line)"; say "$(serve_line)"
first=$("$cli" --state-dir "$state" status --local | jq -r '.serve.pid // empty')

say "== 3. quit the app: sharing keeps running"
quit_app; sleep 2
say "$(agent_line)"; say "$(serve_line)"
[ -n "$first" ] && kill -0 "$first" 2>/dev/null && say "serve pid $first still running: PASS" || say "serve gone: FAIL"

say "== 4. reopen: the app picks the running host back up"
launch; say "$(report)"

say "== 5. Stop Sharing: unregistered, stopped, and stays stopped"
post stop-sharing
wait_for "Sharing stopped" && say "$(report)" || say "FAIL: not stopped: $(report)"
say "$(agent_line)"; say "$(serve_line)"
sleep 15
say "15 s later: $(agent_line); $(serve_line)"
quit_app

say "== 6. a latch serve started in a terminal: shown as outside the app, never touched"
"$cli" --state-dir "$state" serve --no-dashboard > /dev/null 2>&1 &
terminal_serve=$!
sleep 2
launch
wait_for "Sharing outside the app" && say "$(report)" || say "FAIL: $(report)"
post start-sharing; sleep 2
say "after Start Sharing: $(report)"
post stop-sharing; sleep 2
say "after Stop Sharing: $(report)"
say "$(agent_line)"
kill -0 "$terminal_serve" 2>/dev/null && say "terminal serve pid $terminal_serve untouched: PASS" || say "terminal serve gone: FAIL"
kill "$terminal_serve"; terminal_serve=""
quit_app

say "== 7. a serve that fails to start (its saved folder is gone): Start reports it, unregisters, and launchd doesn't retry"
mkdir -p "$dev/sharing-gone"
"$cli" --state-dir "$state" shares set "$dev/sharing-gone" > /dev/null
rmdir "$dev/sharing-gone"
launch
post start-sharing; sleep 6
say "$(report)"
say "$(agent_line)"; say "$(serve_line)"
sleep 12
say "12 s later: $(agent_line); $(serve_line)"
"$cli" --state-dir "$state" shares set "$share" > /dev/null
say "-- 7b. with the folder back, Start Sharing again serves (latch still reports the failed run until then)"
say "latch status before: $(serve_line)"
post start-sharing
wait_for "Sharing" && say "$(report)" || say "FAIL: not serving after a failed start: $(report)"
post stop-sharing
wait_for "Sharing stopped" > /dev/null || say "FAIL: not stopped: $(report)"
quit_app

say "== 8. an app on a different state folder from its agent: Start refuses, registers nothing"
args=(-LatchStateDir "$state-other" -ApplePersistenceIgnoreState YES -Onboarded YES)
mkdir -p "$state-other"
launch
post start-sharing; sleep 2
say "$(report)"
say "$(agent_line)"
quit_app
args=(-LatchStateDir "$state" -ApplePersistenceIgnoreState YES -Onboarded YES)
rm -rf "$state-other"
