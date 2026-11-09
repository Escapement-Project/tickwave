# chrono's decode benchmark

Rust/WASM decoding against JavaScript decoding, on real recordings, in Chromium and Firefox. The
numbers and what they mean are in `RESULTS.md`; this file says how to get them again.

`pages-range/` is the chrono session's Pages range check, not part of this harness.

## What is here

| Path | What |
| --- | --- |
| `src/format.ts` | A reader for FORMAT.md written from the spec alone: head, index, overview, blocks |
| `src/zstd.ts` | fzstd and `DecompressionStream("zstd")` inflates, with FORMAT.md's checksum rule |
| `src/decoders.ts` | The decoders under test behind one interface |
| `src/worker.ts`, `src/entry/` | The decode worker, one bundle a decoder |
| `src/page-inset.ts`, `src/draw.ts` | The cold first-inset page and its stand-in drawing |
| `src/page-bench.ts` | The block-decode page the runner drives |
| `src/canonical.ts` | The bytes each decoded block is hashed over (SHA-256) |
| `libzstd/` | chrono-wasm's decoder surface with libzstd (zstd-sys) for every inflate; its `probe` feature times inflate and parse apart |
| `native/` | The native baseline: hashes, corrupted blocks, native timings |
| `scripts/run.ts` | Runs the measures through Playwright and writes `results/raw/` |
| `scripts/report.ts` | Turns `results/raw/` (and `results/native/`) into the tables |
| `scripts/serve.ts` | The static server: gzip for .js/.wasm/.html, byte ranges, cross-origin isolation |
| `scripts/cpu.ts` | The machine's CPU busy fraction, and the gate each timed unit waits behind |
| `scripts/cold-check.ts` | A fresh browser against a new context, for the decoder's compile time |

The decoders:

| Label | Bundle | What |
| --- | --- | --- |
| `ruzstd-full` | `dec/ruzstd-full` | `crates/wasm`, default features (with the sidecar parser) |
| `ruzstd` | `dec/ruzstd` | `crates/wasm --no-default-features` (decoder only) |
| `ruzstd#2` | `dec/ruzstd` | the same, run as a second decoder: the A-A pair |
| `libzstd` | `dec/libzstd` | `bench/libzstd`: `parse_with`, `decode_block_with` and `decode_overview_block_with` with libzstd; no ruzstd |
| `fzstd` | `dec/fzstd` | fzstd, `src/format.ts`, hash-wasm's XXH64 for the checksum |
| `fzstd-jsxxh` | `dec/fzstd-jsxxh` | the same with a pure-JavaScript XXH64 (`@intrnl/xxhash64`) |
| `stream` | `dec/stream` | `DecompressionStream("zstd")` and `src/format.ts` |
| `probe-libzstd` | `dec/probe-libzstd` | `bench/libzstd --features probe`: inflate and parse timed apart |
| `probe-ruzstd` | `dec/probe-ruzstd` | the same with chrono-format's ruzstd `inflate` (`--no-default-features --features probe`) |

## Needs

- Rust with the `wasm32-unknown-unknown` target; `wasm-bindgen` at the version `wasm-bindgen` in
  `Cargo.lock` names (0.2.130 when written); `wasm-opt` (binaryen).
- clang with a wasm32 target, for zstd-sys to compile libzstd. cc-rs found it, and an archiver,
  with no configuration on this machine, which has `llvm-ar`.
- Node 24 or later (it runs the TypeScript scripts directly) and npm.
- The converted recordings, which are not in this repository. `reference.sh` and
  `scripts/serve.ts` read them from the folder the `CONVERTED` environment variable names.

## Rerun everything

From `bench/`:

```sh
./build.sh          # both chrono-wasm builds, the libzstd and probe builds, the native tool, the bundles
./reference.sh      # native hashes of every block and overview block, and the corrupted blocks
node scripts/run.ts # every measure in both browsers
node scripts/cpu.ts && target/release/chrono-bench-native time --runs 20 --out results/native \
  $(printf "$CONVERTED/%s.1024.chrono " gcc-swd-reads stock-reads \
    swo-24000000 gcc-oscan1-probe-rs gcc-swd-matrix-capture100000 session50)
node scripts/report.ts > results/tables.md
```

`build.sh` builds chrono-wasm from `crates/wasm` with `--locked` into `bench/target/`, so it never
touches the chrono workspace's `target/` or `crates/wasm/pkg/`. It records the chrono commit in
`build/base.txt` and any uncommitted `crates/format` and `crates/wasm` changes in
`build/decoder.diff`. The runner writes both, with the SHA-256 of every module it serves, to
`results/raw/machine.json`. Run `build.sh` again after `crates/format` or `crates/wasm` change,
then `reference.sh`.

`scripts/run.ts` takes `--measures` (any of `correct,inset,blocks,whole,checksum`),
`--browsers chromium,firefox`, `--runs 20`, `--recs` (comma-separated recording names, without
`.chrono`) and `--decoders` (labels from the table above, in place of each measure's own list).
Each measure writes `results/raw/<measure>.<browser>.json` as it goes. Every run
first checks that each recording is the one `reference.sh` hashed, and checks again at the end;
a recording that changed fails the run.

Other work runs on this machine, so the timings wait for it at two levels. Before each timed
measure the runner waits until the 1-minute load average has stayed below 2 for five minutes.
Before each timed unit (a cold inset, a pass over a recording's blocks, a whole-file decode) it
waits until three seconds in a row have the CPUs less than 10 % busy. Neither gives up; each says
in the log every 10 minutes that it is still waiting. Each unit records the busy fraction while it
ran, `results/raw/cpu.json` keeps every second's, and each result file has the load average at
its start and end. `node scripts/cpu.ts` runs both gates for other commands, such as the native
timing: `node scripts/cpu.ts && target/release/chrono-bench-native time ...`.

The first-inset measure launches a fresh browser for every run. In Firefox, a new context in a
running browser is not cold for a WASM decoder: the first module a process compiles takes about
50 ms longer than later ones. `node scripts/cold-check.ts [decoder] [runs]` shows it.

| Measure | What it does |
| --- | --- |
| `correct` | Every block and overview block of every recording through every decoder, hashed and compared with the native hashes; every corrupted block through every decoder |
| `inset` | Cold first inset, a fresh browser a run; Chromium also under DevTools' "Slow 4G" |
| `blocks` | Every block of every recording, each decoder in its own worker, 20 alternating runs after one discarded warm-up run |
| `checksum` | The checksum step alone, hash-wasm against the pure-JavaScript XXH64, on `stock-reads` |
| `whole` | `gcc-swd-matrix-capture100000` decoded block after block inside one worker |

Quicker checks:

```sh
node scripts/smoke.ts [recording.chrono]          # one cold inset per decoder, both browsers
node scripts/ts-sums.ts FILE.chrono               # the TypeScript reader's per-channel sums...
node ../crates/wasm/check.mjs FILE.chrono         # ...against chrono-wasm's
node scripts/serve.ts 8080                        # then http://127.0.0.1:8080/inset.html?dec=fzstd&rec=/rec/gcc-swd-reads.1024.chrono
npx tsc -p .                                      # type-check
cargo clippy -p chrono-bench-native -p chrono-wasm-libzstd -- -D warnings
```

## Firefox and `DecompressionStream("zstd")`

Firefox 159 has the zstd format behind `dom.compression_streams.zstd.enabled`, off by default.
The runner launches Firefox with it on, so the `stream` decoder can be measured; the `correct`
measure also records what each browser offers with default settings.
