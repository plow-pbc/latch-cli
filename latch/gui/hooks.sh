#!/bin/sh
# Drives a test-hooks build of the Mac app (App/TestHooks.swift) from the
# command line, for a local check of launch, the flow's state and what it
# records. Each command is a model intent: `primary` calls the flow's
# primary action, as its button would, but no button, focus or keyboard is
# involved, so this is not a click test.
#
#   hooks.sh build [--window]   the test app, .dev/hooks/Plow Latch Hooks.app
#   hooks.sh open [--fixture SPEC]
#                               opens it on a fresh .dev/tests/hooks/state;
#                               with a fixture, on made-up data instead of
#                               latch's, in the main window (needs --window):
#                               SPEC is "hosts=3,records=50,vault=100,agents=2"
#                               (the defaults) plus "burst=N" new records
#                               at once and "rate=N" a second after the
#                               first read, "delay=MS" per read (and up to
#                               "jitter=MS" more, at random),
#                               "connectors=MS" a plugin change or Google
#                               sign-in takes, which then hold, and
#                               "viewer=1" for this Mac viewing, not hosting
#   hooks.sh send COMMAND [ARG] one hook (TestHooks.swift lists them)
#   hooks.sh state              prints the flow's state as the app records it
#   hooks.sh quit               quits it and deletes its preferences
#
# The test app has its own bundle id, co.plow.latch.hooks, so it never
# shares the installed or dev app's grants or preferences, and runs this
# checkout's debug latch with no Plow baked in. Built without --window it
# is background-only: it opens no window and shows no Dock icon, and its
# flow starts without one, so a screen's own work (Log in starting a login
# as it appears) does not run; the commands that snapshot or resize a
# window need --window, which puts the window on screen.
set -eu
gui=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$gui/../.." && pwd)
id=co.plow.latch.hooks
dir="$root/.dev/hooks"
app="$dir/Plow Latch Hooks.app"
state="$root/.dev/tests/hooks/state"
exe="$app/Contents/MacOS/latch"
die() { echo "hooks.sh: $*" >&2; exit 1; }
# The app itself; its latch's commands run the same binary with more words.
pid() { pgrep -fx "$exe --state-dir $state" || true; }
send() { "$dir/hooks" post "$id" "$@"; }
# The test app's preferences (which screens were passed), the empty file
# cfprefsd leaves behind once they are deleted, and its windows' saved
# state.
forget() {
  defaults delete "$id" >/dev/null 2>&1 || true
  rm -f "$HOME/Library/Preferences/$id.plist"
  rm -rf "$HOME/Library/Saved Application State/$id.savedState"
}
# The flow's state, asked for again until it comes: the app hears nothing
# until it has set up its hooks.
record() {
  rm -f "$state.json"
  for _ in $(seq 20); do
    send state "$state.json"
    sleep 0.25
    [ ! -f "$state.json" ] || return 0
  done
  die "the test app wrote no state"
}

case "${1:-}" in
build)
  [ $# -le 2 ] && { [ $# -eq 1 ] || [ "$2" = --window ]; } || die "usage: hooks.sh build [--window]"
  mkdir -p "$dir"
  swiftc -O "$gui/hooks.swift" -o "$dir/hooks"
  (cd "$gui/.." && cargo build --locked --quiet)
  "$gui/build.sh" --test-hooks >/dev/null
  version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$gui/../Cargo.toml" | head -1)
  sh "$root/scripts/macos-app-bundle.sh" --id "$id" --name "Plow Latch Hooks" "$version" \
    "${CARGO_TARGET_DIR:-$gui/../target}/debug/latch" "$app"
  # Never left where a real bundle would pick it up.
  rm -f "$gui/dist/libPlowLatchUI.dylib"
  [ $# -eq 2 ] || plutil -insert LSBackgroundOnly -bool YES "$app/Contents/Info.plist"
  sh "$root/scripts/macos-sign-app.sh" "$app"
  echo "$app"
  ;;
open)
  fixture=""
  if [ $# -eq 3 ] && [ "$2" = --fixture ]; then fixture=$3; else [ $# -eq 1 ] || die "usage: hooks.sh open [--fixture SPEC]"; fi
  [ -d "$app" ] || die "no test app: run hooks.sh build first"
  [ -z "$(pid)" ] || die "the test app is already open: hooks.sh quit first"
  rm -rf "$state" "$state.json"
  mkdir -p "$state"
  chmod 700 "$state"
  forget
  if [ -n "$fixture" ]; then
    open -g "$app" --env "LATCH_HOOKS_FIXTURE=$fixture" --args --state-dir "$state"
  else
    open -g "$app" --args --state-dir "$state"
  fi
  for _ in $(seq 50); do
    [ -z "$(pid)" ] || break
    sleep 0.1
  done
  [ -n "$(pid)" ] || die "the test app did not start"
  record
  if /usr/libexec/PlistBuddy -c 'Print :LSBackgroundOnly' "$app/Contents/Info.plist" >/dev/null 2>&1; then
    windows=$("$dir/hooks" onscreen "$(pid)")
    [ "$windows" = 0 ] || { kill "$(pid)"; die "the background-only test app put $windows windows on screen"; }
  fi
  echo "opened on $state (pid $(pid))"
  ;;
send)
  [ $# -ge 2 ] && [ $# -le 3 ] || die "usage: hooks.sh send COMMAND [ARG]"
  [ -n "$(pid)" ] || die "the test app is not open"
  shift
  send "$@"
  ;;
state)
  [ -n "$(pid)" ] || die "the test app is not open"
  record
  cat "$state.json"
  echo
  ;;
quit)
  app_pid=$(pid)
  if [ -n "$app_pid" ]; then
    send quit
    for _ in $(seq 50); do
      kill -0 "$app_pid" 2>/dev/null || break
      sleep 0.1
    done
    ! kill -0 "$app_pid" 2>/dev/null || die "the test app did not quit (pid $app_pid)"
  fi
  forget
  rm -rf "$state" "$state.json"
  ;;
*)
  sed -n '2,/^set -eu/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2
  exit 2
  ;;
esac
