#!/bin/sh
# Builds the phone page into dist/: the shim's files, and iroh for the
# browser with its JavaScript bindings. The wasm-bindgen CLI, at the
# crate's exact version, goes in the repo's .dev/tools.
set -eu
cd "$(dirname "$0")"
host=$(rustc -vV | sed -n 's/^host: //p')
# Apple's `ar` writes wasm archives the linker cannot read; llvm-ar from
# rust-toolchain.toml's llvm-tools can.
export AR_wasm32_unknown_unknown="$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-ar"
cargo build --locked --release --target wasm32-unknown-unknown
version=$(sed -n '/^name = "wasm-bindgen"$/{n;s/^version = "\(.*\)"$/\1/p;}' Cargo.lock)
tools=../../.dev/tools
if [ "$("$tools/bin/wasm-bindgen" --version 2>/dev/null)" != "wasm-bindgen $version" ]; then
    cargo install --locked --root "$tools" wasm-bindgen-cli --version "$version"
fi
rm -rf dist
"$tools/bin/wasm-bindgen" target/wasm32-unknown-unknown/release/latch_phone.wasm \
    --out-dir dist --target web --no-typescript
cp web/* dist/
# The page's policy lets it reach one Plow, to sign the owner in:
# https://api.plow.co, or for a local or dev Plow the origin PLOW_API names
# (PLOW_API=http://127.0.0.1:18904), which then replaces it.
if [ -n "${PLOW_API:-}" ]; then
    printf '%s\n' "$PLOW_API" | grep -Eq '^https?://[A-Za-z0-9.-]+(:[0-9]+)?$' \
        || { echo "PLOW_API is an origin, such as http://127.0.0.1:18904, not $PLOW_API" >&2; exit 1; }
    sed "s|https://api.plow.co|$PLOW_API|" dist/index.html > dist/index.html.new
    mv dist/index.html.new dist/index.html
fi
# A page served under a path with no trailing slash (plow.co/latch/phone)
# names its folder as its base, so its files load from there: BASE=/latch/phone/.
if [ -n "${BASE:-}" ]; then
    awk -v base="$BASE" '{ print } /<head>/ { print "<base href=\"" base "\">" }' dist/index.html > dist/index.html.new
    mv dist/index.html.new dist/index.html
fi
ls -l dist
