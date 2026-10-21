#!/bin/sh
# Builds a static Linux (musl) latch binary inside the pinned rust:alpine
# image, for CI and for local testing alike.
#
#   ./build-musl.sh amd64|arm64    ->  target/musl-<arch>/release/latch
#
# It ends by running the fresh binary's `--help` in the same container.
# LATCH_BUILD_STATE_DIR, LATCH_BUILD_API_URL and LATCH_BUILD_CHANNEL, if
# set, are passed through to bake them into the build (see `src/cli.rs`). The
# container has no git, so the commit `--version` names is taken here.
#
# Alpine's Rust targets musl and links statically by default, so the binary
# runs on any Linux (Debian, Alpine, ...) without a matching libc.
set -eu
arch="$1"
image="rust:alpine@sha256:0cce0a5e0e8ba67b455257a3a02a1d99005f382748789d6464460028810f1627"
cd "$(dirname "$0")"
LATCH_BUILD_COMMIT="$(git rev-parse --short=9 HEAD)"
if [ -n "$(git status --porcelain --untracked-files=no -- .)" ]; then
  LATCH_BUILD_COMMIT="$LATCH_BUILD_COMMIT-dirty"
fi
export LATCH_BUILD_COMMIT
docker run --rm --platform "linux/$arch" \
  -v "$PWD:/src" -w /src \
  -e CARGO_HOME=/src/target/cargo-home \
  -e CARGO_TARGET_DIR="/src/target/musl-$arch" \
  -e LATCH_BUILD_STATE_DIR -e LATCH_BUILD_API_URL -e LATCH_BUILD_CHANNEL -e LATCH_BUILD_COMMIT \
  "$image" \
  sh -c 'apk add --no-cache musl-dev >/dev/null && cargo build --release --locked &&
    "$CARGO_TARGET_DIR/release/latch" status --help >/dev/null'
