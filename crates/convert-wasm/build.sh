#!/usr/bin/env bash
# Builds the browser's converter into crates/convert-wasm/pkg: an ES module and its .wasm. As for
# the decoder (crates/wasm/build.sh), clang must have the wasm32 target for libzstd.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -p escapement-tickwave-convert-wasm --target wasm32-unknown-unknown --profile wasm
wasm-bindgen --target web --out-dir crates/convert-wasm/pkg target/wasm32-unknown-unknown/wasm/escapement_tickwave_convert_wasm.wasm
wasm-opt --remove-exports --pass-arg='remove-exports@rust_zstd_wasm_shim_*' \
  -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
  crates/convert-wasm/pkg/escapement_tickwave_convert_wasm_bg.wasm -o crates/convert-wasm/pkg/escapement_tickwave_convert_wasm_bg.wasm
