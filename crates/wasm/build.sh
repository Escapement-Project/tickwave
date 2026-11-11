#!/usr/bin/env bash
# Builds the browser's decoder into crates/wasm/pkg: an ES module and its .wasm. zstd-sys compiles
# libzstd for wasm32 with clang, so clang must have the wasm32 target.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -p escapement-tickwave-wasm --target wasm32-unknown-unknown --profile wasm
wasm-bindgen --target web --out-dir crates/wasm/pkg target/wasm32-unknown-unknown/wasm/escapement_tickwave_wasm.wasm
# zstd-sys's wasm shim gives libzstd its libc as #[no_mangle] functions, which the module would
# export. Unexported, they go wherever libzstd's decoder does not call them (its qsort, for one).
wasm-opt --remove-exports --pass-arg='remove-exports@rust_zstd_wasm_shim_*' \
  -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
  crates/wasm/pkg/escapement_tickwave_wasm_bg.wasm -o crates/wasm/pkg/escapement_tickwave_wasm_bg.wasm
