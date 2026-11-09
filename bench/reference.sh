#!/usr/bin/env bash
# The native baseline every browser decoder is checked against, and the corrupted blocks every
# decoder must refuse, from the converted recordings. Run after build.sh.
set -euo pipefail
cd "$(dirname "$0")"
converted=${CONVERTED:?set CONVERTED to the folder of converted recordings (README.md, Needs)}
recs=(gcc-swd-reads.1024 stock-reads.1024 swo-24000000.1024 gcc-oscan1-probe-rs.1024
  gcc-swd-matrix-capture100000.1024 session50.1024)
native=target/release/tickwave-bench-native
rm -rf build/reference build/corrupt
$native hashes --out build/reference "$converted/identify-swd.tickwave" $(printf "$converted/%s.tickwave " "${recs[@]}")
for r in gcc-swd-reads.1024 session50.1024; do
  $native corrupt --out build/corrupt "$converted/$r.tickwave"
done
