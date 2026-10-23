#!/bin/sh
# Runs a macOS archive built on macOS 26 on an older Mac, where nothing else
# tests it: latch's commands that need no network or Plow, on a fresh state
# folder; the Messages reader, up to a database it is not allowed to read;
# and the app, opened as Finder opens it, which on macOS before 26 shows the
# alert that its window needs macOS 26 instead of crashing. Mac CI runs it on
# the oldest macOS images GitHub offers.
#   macos-smoke.sh DIR    (an unpacked latch-VERSION-TARGET archive)
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
. "$here/latch/macos/identity.sh"
dir=$(cd "$1" && pwd)
app="$dir/$LATCH_APP_NAME.app"
fail() {
  echo "macos-smoke: $*" >&2
  exit 1
}
macos=$(sw_vers -productVersion)
[ "${macos%%.*}" -lt 26 ] || fail "macOS $macos runs the window itself; run this on macOS 25 or earlier"
echo "macOS $macos on $(uname -m)"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Runs a command, prints what it said, and fails unless it exits `status`
# and its output has `text` in it.
expect() {
  status=$1 text=$2
  shift 2
  echo "\$ $*"
  set +e
  "$@" >"$work/out" 2>&1
  got=$?
  set -e
  cat "$work/out"
  [ "$got" = "$status" ] || fail "$1 exited $got, not $status"
  grep -qF -- "$text" "$work/out" || fail "$1 did not say $text"
}

# latch, through the link the installer puts on PATH.
state="$work/state"
expect 0 'protocol plow/latch/' "$dir/latch" --version
expect 0 '"state":"stopped"' "$dir/latch" --state-dir "$state" status --local
expect 0 '"name":"messages"' "$dir/latch" --state-dir "$state" status --local
expect 0 '"records":0' "$dir/latch" --state-dir "$state" audit verify
expect 0 '"exit_codes"' "$dir/latch" help --json

# The Messages reader, on a database it may not read, as on a Mac without
# Full Disk Access (never this Mac's own: a runner may grant it).
expect 0 'USAGE' "$dir/latch-messages" --help
: >"$work/chat.db"
chmod 000 "$work/chat.db"
expect 1 '"code":"messages_not_granted"' "$dir/latch-messages" --store "$work/chat.db" chats

# The app, opened by LaunchServices. Below macOS 26 latch shows its "needs
# macOS 26" alert and waits there for OK; it never loads the window's
# library. A crash, or a load of that library, fails.
crashes() { ls "$HOME/Library/Logs/DiagnosticReports" 2>/dev/null | grep -c '^latch' || true; }
before=$(crashes)
echo "\$ open '$app'"
open "$app"
executable="$app/Contents/MacOS/latch"
pid=""
for _ in $(seq 40); do
  pid=$(pgrep -f "^$executable\$" || true)
  [ -n "$pid" ] && break
  sleep 0.25
done
[ -n "$pid" ] || fail "the app did not start, or quit at once"
sleep 3
kill -0 "$pid" 2>/dev/null || fail "the app quit instead of showing its alert"
sample "$pid" 1 -file "$work/sample" >/dev/null 2>&1 || fail "could not sample the app"
grep -q CFUserNotificationDisplayAlert "$work/sample" || {
  cat "$work/sample"
  fail "the app is not showing the needs-macOS-26 alert"
}
if grep -q libPlowLatchUI "$work/sample"; then
  fail "the app loaded its window's library below macOS 26"
fi
kill "$pid"
[ "$(crashes)" = "$before" ] || fail "latch crashed: see ~/Library/Logs/DiagnosticReports"
echo "the app shows its needs-macOS-26 alert"
echo "macos-smoke: ok"
