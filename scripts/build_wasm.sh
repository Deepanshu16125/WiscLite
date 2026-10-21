#!/usr/bin/env bash
# Build the real engine for the browser: wasm32 + wasm-bindgen (no-modules, works in pages and workers).
# Needs: the wasm32-unknown-unknown std (rustup target add, or Debian libstd-rust-dev-wasm32),
#        wasm-ld (lld), and wasm-bindgen-cli =0.2.100 (cargo install wasm-bindgen-cli --version 0.2.100).
set -euo pipefail
cd "$(dirname "$0")/../wasm"
cargo test --release
cargo build --release --target wasm32-unknown-unknown
wasm-bindgen --target no-modules --no-typescript --out-dir ../web/pkg target/wasm32-unknown-unknown/release/wisclite_wasm.wasm
ls -la ../web/pkg
