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
ls -l dist
