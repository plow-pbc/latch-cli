#!/bin/sh
# Builds "Plow Latch.app" into apps/macos/build/, ad-hoc signed, with a latch
# inside it at Contents/Helpers/latch.
#   scripts/build-app.sh [--test-hooks] [--latch PATH]
# --test-hooks  compiles in the hooks the measurement scripts drive
# --latch PATH  bundles this latch instead of building the repo's (release)
set -eu
cd "$(dirname "$0")/.."
hooks=""
latch=""
while [ $# -gt 0 ]; do
    case "$1" in
        --test-hooks) hooks="-Xswiftc -DLATCH_TEST_HOOKS" ;;
        --latch) latch="$2"; shift ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
    shift
done

if [ -z "$latch" ]; then
    (cd ../../latch && cargo build --locked --release)
    latch="${CARGO_TARGET_DIR:-../../latch/target}/release/latch"
fi

# shellcheck disable=SC2086
swift build -c release --product PlowLatch $hooks
bin="$(swift build -c release --show-bin-path)"

app="build/Plow Latch.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers"
cp "$bin/PlowLatch" "$app/Contents/MacOS/PlowLatch"
cp "$latch" "$app/Contents/Helpers/latch"
cp Info.plist "$app/Contents/Info.plist"
codesign --force --sign - "$app/Contents/Helpers/latch"
codesign --force --sign - "$app"
echo "$app"
