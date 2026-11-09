#!/usr/bin/env bash
# Builds the browser's decoder into crates/wasm/pkg: an ES module and its .wasm. zstd-sys compiles
# libzstd for wasm32 with clang, so clang must have the wasm32 target.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -p tickwave-wasm --target wasm32-unknown-unknown --profile wasm
wasm-bindgen --target web --out-dir crates/wasm/pkg target/wasm32-unknown-unknown/wasm/tickwave_wasm.wasm
wasm-opt -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
  crates/wasm/pkg/tickwave_wasm_bg.wasm -o crates/wasm/pkg/tickwave_wasm_bg.wasm
