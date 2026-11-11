#!/usr/bin/env bash
# Builds what the benchmark runs: both escapement-tickwave-wasm builds (from crates/wasm, into
# bench's own target directories, so the tickwave workspace's target/ and crates/wasm/pkg stay
# untouched), the libzstd build, the native reference tool, and the bundled workers and pages in
# build/www.
set -euo pipefail
cd "$(dirname "$0")"
bench=$PWD
# crates/wasm/build.sh's wasm-opt flags.
opt=(--remove-exports --pass-arg='remove-exports@rust_zstd_wasm_shim_*'
  -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext)

rm -rf build/www build/glue
mkdir -p build/www/dec build/glue
# The decoder as built: the base commit and the uncommitted changes to it, enough to rebuild it.
# staged.diff is the part already staged for the next commit.
git -C .. rev-parse HEAD > build/base.txt
git -C .. diff HEAD -- crates/format crates/wasm > build/decoder.diff
git -C .. diff --cached -- crates/format crates/wasm > build/staged.diff

wasm() { # name, built .wasm, glue base name
  wasm-bindgen --target web --out-dir "build/glue/$1" "$2"
  mkdir -p "build/www/dec/$1"
  wasm-opt "${opt[@]}" "build/glue/$1/$3_bg.wasm" -o "build/www/dec/$1/$3_bg.wasm"
}

for v in full decoder; do
  features=()
  [ "$v" = decoder ] && features=(--no-default-features)
  (cd .. && cargo build --locked -p escapement-tickwave-wasm --target wasm32-unknown-unknown --profile wasm \
    "${features[@]}" --target-dir "$bench/target/escapement-tickwave-wasm-$v")
done
wasm ruzstd-full target/escapement-tickwave-wasm-full/wasm32-unknown-unknown/wasm/escapement_tickwave_wasm.wasm escapement_tickwave_wasm
wasm ruzstd target/escapement-tickwave-wasm-decoder/wasm32-unknown-unknown/wasm/escapement_tickwave_wasm.wasm escapement_tickwave_wasm

# zstd-sys compiles libzstd for this target with clang (cc-rs picks it), against its own
# wasm-shim headers.
cargo build --locked -p escapement-tickwave-wasm-libzstd --target wasm32-unknown-unknown --profile wasm
wasm libzstd target/wasm32-unknown-unknown/wasm/escapement_tickwave_wasm_libzstd.wasm escapement_tickwave_wasm_libzstd

# The probe builds: inflate and parse timed apart, with libzstd and with ruzstd.
for v in libzstd ruzstd; do
  features=(--features probe)
  [ "$v" = ruzstd ] && features=(--no-default-features --features probe)
  cargo build --locked -p escapement-tickwave-wasm-libzstd --target wasm32-unknown-unknown --profile wasm \
    "${features[@]}" --target-dir "target/probe-$v"
  wasm "probe-$v" "target/probe-$v/wasm32-unknown-unknown/wasm/escapement_tickwave_wasm_libzstd.wasm" escapement_tickwave_wasm_libzstd
done

cargo build --locked --release -p escapement-tickwave-bench-native

npm ci --silent
node scripts/bundle.ts
