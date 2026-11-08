#!/usr/bin/env bash
# Proves the check against local servers before trusting it on a real host: it must pass where
# ranges work and fail where the server ignores Range or ranges over gzipped bytes.
#   ./dry-run.sh SITE_DIR
set -uo pipefail
site=$1
cd "$(dirname "$0")"
summarise() {
  uv run -q python - "$1" <<'PY'
import json, sys
for run in json.load(open(sys.argv[1])):
    rows = run["rows"]
    bad = sorted({(r.get("status"), r.get("encoding"), (r.get("error") or "")[:40]) for r in rows if not r["pass"]}, key=str)
    print(f"  {run['browser']:8} {run['origin']:12} {sum(r['pass'] for r in rows):2} of {len(rows)} pass", bad[:3])
PY
}
for mode in ranges whole gzip; do
  node serve.mjs "$site" 8932 "$mode" &
  server=$!
  for _ in $(seq 1 50); do curl -s -o /dev/null http://127.0.0.1:8932/files.json && break; done
  echo "== $mode"
  if WAIT_MS=60000 timeout 300 node run.mjs http://127.0.0.1:8932/ > "results-$mode.json" 2> run.err; then
    summarise "results-$mode.json"
  else
    tail -3 run.err
  fi
  kill "$server"
  wait "$server" 2>/dev/null
done
