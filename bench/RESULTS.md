# tickwave decode benchmark: results

These runs were made before chrono was renamed tickwave. The names here are the new ones, and the
figures stand.

Rust/WASM decoding against JavaScript decoding, on the protocol in ROUND.md. `README.md` says how
to rerun every number here.

The decoders were built from tickwave `48d7f58`, with no uncommitted changes to `crates/format` or
`crates/wasm`. The recordings are the ones reconverted with overview blocks on 2026-11-08 at 23:50.
Since the run, `c8b1b17` changed the sidecar resolution: at `6b82dda` the default build is 527,039
bytes, 3,737 more gzipped than the one measured (zlib level 9, as `bundle.ts` compresses), about
21 ms more on Slow 4G. The decoder-only and libzstd modules and every worker bundle build
byte-identical at `6b82dda`.

## What the numbers say

An early draft of PLAN.md §7 proposed a bar for a second decoder in JavaScript: it reaches the
first inset more than 50 ms sooner on the throttled profile, or it decodes blocks faster than WASM.
The user set no bar in advance; they decide from these numbers. The tables below still mark the
draft bar, for reference only. The JavaScript decoder here is fzstd with the TypeScript reader.

- **First inset on Slow 4G.** fzstd draws the first inset 545-558 ms sooner than tickwave-wasm's
  decoder-only build on all six recordings, and 1,088-1,107 ms sooner than its default build. The
  A-A floor is 0.2 %, under 10 ms. The gap is the decoder's own download: 112 KB gzipped for the
  decoder-only build and 210 KB for the default, against 13 KB for fzstd's worker, on a link that
  carries 180 KB a second. fzstd's decoder is ready at 1.8 s; tickwave-wasm's at 3.3 s and 3.8 s.
  Only Chromium was throttled.
- **First inset, unthrottled.** In Chromium, fzstd is within the 3.4 % floor on four recordings
  and 7-12 % (5 and 8 ms) later on two. In Firefox, the two are within the 2.6 % floor on four
  recordings; fzstd is 5 % sooner on one and 4 % later on another. A fresh Firefox takes about
  47 ms over its first fetch, the head's, and tickwave-wasm compiles in that time.
- **Block decode depends on the browser.** In Chromium, tickwave-wasm is faster on every recording;
  fzstd is 11-35 % slower. In Firefox, fzstd is faster on four of six (11-39 %) and slower on two:
  gcc-oscan1-probe-rs by 5 % and session50 by 12 %. The A-A floors are 2.2 % and 1.5 %. The whole
  largest recording takes 4.42 s with tickwave-wasm and 6.75 s with fzstd in Chromium, and 5.13 s
  and 4.73 s in Firefox.

Beside the bar:

- **libzstd in WASM** decodes session50's analog codes 3.9 times as fast as ruzstd in Chromium and
  4.4 times in Firefox. On the digital recordings it ranges from 12 % slower to 5 % faster in
  Chromium, and is 9-50 % faster in Firefox. Its module is 136 KB gzipped, 24 KB more than the
  decoder-only build, because it carries ruzstd as well for the head. On Slow 4G that costs it
  2-4 % against the decoder-only build.
- **The sidecar parser** in the default build costs 540-548 ms on Slow 4G. Unthrottled it is
  within the floor on 11 of 12 rows, and 3 ms (4 %) later on the other.
- **DecompressionStream("zstd")** is the fastest decoder in Firefox on five of six recordings, but
  only behind a pref. Chromium does not have it.
- **The worker hop** costs 0.1-0.2 ms a block. Decoding one takes 5-8 ms (the medians of its
  steps, summed).
- **Correctness.** In both browsers, every decoder matches the native decoder on all 1,505 blocks,
  and every decoder that can decode overview blocks matches it on all 610 of them. The native
  decoder accepts bytes after a block's frame, and with them a corrupted block. tickwave-wasm
  inherits that (see "A defect in the native decoder" below).

## How it was measured

Machine: AMD Ryzen AI Max+ 395 (16 cores, 32 threads), Linux 7.3.1. Browsers through Playwright
1.65.0, headless: Chromium 158.0.8127.18 (Playwright's headless shell, build 1254) and Firefox
159.0 (build 1572). Node 26.13.0.

**Recordings.** The protocol's six converted recordings, zstd 19, 1 MiB
blocks. session50 holds its analog as 12-bit codes in byte planes.

| Recording | Blocks | Overview blocks | Edges | Samples | File |
| --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | 7 | 4 | 6,791,956 | | 0.39 MB |
| stock-reads | 22 | 17 | 14,324,188 | | 1.40 MB |
| swo-24000000 | 48 | 15 | 43,655,997 | | 6.02 MB |
| gcc-oscan1-probe-rs | 303 | 117 | 238,898,043 | | 11.1 MB |
| gcc-swd-matrix-capture100000 | 742 | 452 | 637,277,314 | | 49.4 MB |
| session50 | 382 | 5 | 0 | 200,004,000 | 108 MB |

**Decoders.**

- tickwave-wasm, default build: `crates/wasm` as it builds by default, with the sidecar parser.
  `build.sh` builds it as `crates/wasm/build.sh` does; the module came out byte-identical to the
  tickwave session's `crates/wasm/pkg` build of the same commit.
- tickwave-wasm, decoder only: `--no-default-features`. The reference row in every comparison.
- libzstd in WASM (`bench/libzstd`): tickwave-wasm's decoder surface, with blocks inflated by
  libzstd through `Head::decode_block_with`. zstd-sys compiles libzstd for `wasm32-unknown-unknown`
  with clang against its own shim headers, which hand `malloc`, `memcpy` and `qsort` to Rust.
  Nothing else was needed. `Head::parse` inflates the overview with ruzstd, so this module carries
  both zstd decoders. `decode_overview_block` takes no inflate, so this build cannot decode overview
  blocks.
- fzstd with a TypeScript reader (`src/format.ts`), written from FORMAT.md without reading the
  Rust. fzstd skips the content checksum, so XXH64 from hash-wasm (a small WASM module) checks it
  as a separate step.
- `DecompressionStream("zstd")` with the same reader. Chromium 158 does not have it. Firefox 159
  has it behind `dom.compression_streams.zstd.enabled`, off by default. The runs turn it on, so
  its rows show what Firefox would do if it shipped. No reader has it today.

Each decoder runs in a Web Worker, as the viewer will. Every bundle is minified by esbuild and
served gzip -9 from a local static server that answers byte ranges. The server isolates the page
(COOP and COEP), so timers resolve to microseconds rather than Firefox's 1 ms.

**First inset.** A fresh browser for every run. In Firefox, a new context in a running browser is
not cold for a WASM decoder. `scripts/cold-check.ts`, 8 runs each: tickwave-wasm is ready in a
median 58.7-60.2 ms in a fresh browser, against 9.0-12.9 ms in a new context. Chromium takes 4-5 ms
either way. Most of that cost falls on the first module a Firefox process compiles, not on each
module: in a probe run once on the build before the overview-blocks change, the default build took
11-23 ms when the libzstd module had compiled first.

The worker starts fetching and compiling its decoder, and fetches the head's first 64 KiB at the
same time. Then it parses the head, fetches the middle block by range and decodes it. The page
draws the block as a polyline, one vertical stroke per pixel column, and reads back one pixel to
force the raster. The time is from navigation start to that point.

Throttled runs use DevTools' "Slow 4G" through the Chrome DevTools Protocol: 562.5 ms latency,
1.6 Mbit/s × 0.9 down, 750 kbit/s × 0.9 up (`front_end/core/sdk/NetworkManager.ts`,
`Slow4GConditions`, devtools-frontend `f439efd2`). It reaches the worker's fetches: a 70,825-byte
block took 963 ms against the 956 ms the profile predicts. Firefox has no equivalent through
Playwright.

**Block decode.** Each decoder has its own worker, all alive at once, each holding the whole
file. One pass decodes every block once. After one discarded pass each, 20 passes alternate
between decoders, with the order rotated each round. The worker times its own decode, and the
page times the round trip from its request to the arrays arriving back.

**Whole file.** The worker decodes every block of gcc-swd-matrix-capture100000 in turn, keeping
nothing, 20 times per decoder in rotation after one discarded run.

**Statistics.** Percentiles are nearest-rank over every sample pooled. For a rate, "p95" means
the slow side: the rate 95 % of samples reach. The A-A pair is the decoder-only tickwave-wasm run as
two decoders. A table's A-A floor is the largest relative gap between the pair's medians over the
table's recordings. A gap to the reference smaller than that floor is printed as "same".

**Other load.** Other sessions build on this machine. Each timed unit (one cold inset, one pass,
one whole-file run) waits until three seconds in a row have the CPUs less than 10 % busy. The
tables give the median and maximum busy fraction while the units ran, which includes the run's own
work, such as launching a browser. A unit under a second counts the second it started in. The
native timing ran behind the same gate, with a load average of 0.94 before and 1.31 after.

## Results

The tables below are `scripts/report.ts`'s output for this run.

Machine: AMD RYZEN AI MAX+ 395 w/ Radeon 8060S, 32 threads; Node v26.13.0; run started 2026-11-09T00:06:27.011Z.
Decoders built from tickwave 48d7f58 with no uncommitted changes to crates/format or crates/wasm. Modules served, SHA-256: libzstd/tickwave_wasm_libzstd_bg.wasm ba096641f6cc3fd6..., ruzstd/tickwave_wasm_bg.wasm 3a0060f992d1e5f3..., ruzstd-full/tickwave_wasm_bg.wasm df0ee3bf3cf6d328....

### Decoder transfer size

| decoder | files | bytes | gzip -9 |
| --- | --- | --- | --- |
| tickwave-wasm, default build (ruzstd) | tickwave_wasm_bg.wasm, worker.js | 530231 | 209697 |
| tickwave-wasm, decoder only (ruzstd) | tickwave_wasm_bg.wasm, worker.js | 271369 | 111913 |
| libzstd in WASM | tickwave_wasm_libzstd_bg.wasm, worker.js | 357954 | 136489 |
| fzstd + TypeScript | worker.js | 30130 | 12559 |
| DecompressionStream("zstd") + TypeScript | worker.js | 13720 | 5219 |

Every decoder also loads the page (inset.html, inset.js: 1219 bytes gzipped).

### Against the draft bar (PLAN.md §7)

A JavaScript decoder (fzstd) against tickwave-wasm. A gap within the A-A floor is "same". The
verdicts apply the early draft's bar, which the user did not adopt.

| measure | recording | medians | A-A floor | verdict |
| --- | --- | --- | --- | --- |
| first inset, Slow 4G, chromium | gcc-swd-reads | fzstd 3732 ms against tickwave-wasm, decoder only (ruzstd) 4283 ms: 551 ms sooner | 7 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | gcc-swd-reads | fzstd 3732 ms against tickwave-wasm, default build (ruzstd) 4824 ms: 1093 ms sooner | 7 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | stock-reads | fzstd 3511 ms against tickwave-wasm, decoder only (ruzstd) 4070 ms: 558 ms sooner | 6 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | stock-reads | fzstd 3511 ms against tickwave-wasm, default build (ruzstd) 4618 ms: 1107 ms sooner | 7 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | swo-24000000 | fzstd 4044 ms against tickwave-wasm, decoder only (ruzstd) 4592 ms: 549 ms sooner | 7 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | swo-24000000 | fzstd 4044 ms against tickwave-wasm, default build (ruzstd) 5134 ms: 1090 ms sooner | 8 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | gcc-oscan1-probe-rs | fzstd 3542 ms against tickwave-wasm, decoder only (ruzstd) 4087 ms: 545 ms sooner | 6 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | gcc-oscan1-probe-rs | fzstd 3542 ms against tickwave-wasm, default build (ruzstd) 4629 ms: 1088 ms sooner | 7 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | gcc-swd-matrix-capture100000 | fzstd 3771 ms against tickwave-wasm, decoder only (ruzstd) 4324 ms: 552 ms sooner | 7 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | gcc-swd-matrix-capture100000 | fzstd 3771 ms against tickwave-wasm, default build (ruzstd) 4866 ms: 1095 ms sooner | 7 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | session50 | fzstd 4817 ms against tickwave-wasm, decoder only (ruzstd) 5372 ms: 555 ms sooner | 8 ms | passes: more than 50 ms sooner |
| first inset, Slow 4G, chromium | session50 | fzstd 4817 ms against tickwave-wasm, default build (ruzstd) 5912 ms: 1095 ms sooner | 9 ms | passes: more than 50 ms sooner |
| block decode in worker, chromium | gcc-swd-reads | fzstd 101.6 against 155.3 M/s | 2.2 % | WASM faster |
| block decode in worker, chromium | stock-reads | fzstd 86.1 against 121.5 M/s | 2.2 % | WASM faster |
| block decode in worker, chromium | swo-24000000 | fzstd 98.5 against 110.4 M/s | 2.2 % | WASM faster |
| block decode in worker, chromium | gcc-oscan1-probe-rs | fzstd 116.2 against 157.5 M/s | 2.2 % | WASM faster |
| block decode in worker, chromium | gcc-swd-matrix-capture100000 | fzstd 107.4 against 145.5 M/s | 2.2 % | WASM faster |
| block decode in worker, chromium | session50 | fzstd 70.9 against 89.6 M/s | 2.2 % | WASM faster |
| block decode in worker, firefox | gcc-swd-reads | fzstd 168.2 against 137.9 M/s | 1.5 % | JavaScript faster |
| block decode in worker, firefox | stock-reads | fzstd 119.3 against 107.2 M/s | 1.5 % | JavaScript faster |
| block decode in worker, firefox | swo-24000000 | fzstd 120.4 against 86.4 M/s | 1.5 % | JavaScript faster |
| block decode in worker, firefox | gcc-oscan1-probe-rs | fzstd 129.8 against 137.0 M/s | 1.5 % | WASM faster |
| block decode in worker, firefox | gcc-swd-matrix-capture100000 | fzstd 138.9 against 122.2 M/s | 1.5 % | JavaScript faster |
| block decode in worker, firefox | session50 | fzstd 62.9 against 71.2 M/s | 1.5 % | WASM faster |

### First inset, chromium 158.0.8127.18, unthrottled

Milliseconds from navigation start to the drawn inset, median (p95) of 20 cold runs. A-A floor 3.4 %; "vs ruzstd" compares medians with the decoder-only build.

| recording | decoder | drawn | worker up | decoder ready | block fetched | decode | vs ruzstd |
| --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, default build (ruzstd) | 74.6 (76.2) | 33.7 | 44.9 | 52.4 | 13.7 | same |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 72.3 (74.9) | 32.2 | 42.6 | 50.2 | 13.9 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 72.6 (75.9) | 32.9 | 43.0 | 50.5 | 13.8 |  |
| gcc-swd-reads | libzstd in WASM | 75.8 (78.8) | 32.2 | 42.7 | 50.8 | 15.3 | +5 % (worse) |
| gcc-swd-reads | fzstd + TypeScript | 73.8 (83.3) | 33.4 | 40.4 | 44.6 | 21.9 | same |
| stock-reads | tickwave-wasm, default build (ruzstd) | 70.1 (75.4) | 32.7 | 44.5 | 53.6 | 9.8 | +4 % (worse) |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 67.1 (70.5) | 33.0 | 43.3 | 51.5 | 9.7 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 69.4 (72.6) | 33.2 | 43.4 | 52.6 | 10.3 |  |
| stock-reads | libzstd in WASM | 71.4 (75.4) | 32.9 | 43.5 | 52.7 | 12.2 | +6 % (worse) |
| stock-reads | fzstd + TypeScript | 65.1 (67.6) | 33.6 | 40.7 | 48.3 | 11.3 | same |
| swo-24000000 | tickwave-wasm, default build (ruzstd) | 72.7 (77.8) | 32.1 | 43.4 | 51.1 | 15.5 | same |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 72.7 (76.1) | 32.7 | 43.4 | 51.1 | 15.1 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 73.4 (75.7) | 32.8 | 42.9 | 51.1 | 15.1 |  |
| swo-24000000 | libzstd in WASM | 74.4 (77.8) | 32.6 | 43.0 | 51.1 | 16.5 | same |
| swo-24000000 | fzstd + TypeScript | 74.3 (76.7) | 33.3 | 40.7 | 45.5 | 22.3 | same |
| gcc-oscan1-probe-rs | tickwave-wasm, default build (ruzstd) | 69.6 (72.6) | 32.0 | 43.5 | 51.0 | 11.6 | same |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 68.9 (72.4) | 32.4 | 42.8 | 50.6 | 11.5 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 67.6 (71.7) | 32.3 | 42.5 | 50.0 | 11.3 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 73.8 (77.8) | 32.3 | 42.8 | 52.1 | 14.1 | +7 % (worse) |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 77.1 (80.8) | 33.8 | 40.8 | 46.6 | 24.2 | +12 % (worse) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, default build (ruzstd) | 74.7 (77.6) | 32.5 | 43.8 | 52.4 | 14.1 | same |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 74.0 (76.6) | 32.6 | 42.5 | 51.3 | 14.9 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 72.7 (76.9) | 32.2 | 41.6 | 50.1 | 14.1 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 75.6 (79.3) | 31.8 | 42.2 | 51.4 | 16.1 | same |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 75.8 (80.2) | 32.3 | 39.6 | 47.6 | 21.0 | same |
| session50 | tickwave-wasm, default build (ruzstd) | 74.1 (78.1) | 32.4 | 43.4 | 53.8 | 9.9 | same |
| session50 | tickwave-wasm, decoder only (ruzstd) | 72.7 (75.7) | 32.2 | 42.2 | 53.2 | 9.5 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 71.5 (76.1) | 32.3 | 42.2 | 52.0 | 9.5 |  |
| session50 | libzstd in WASM | 69.4 (71.7) | 31.8 | 42.2 | 53.0 | 5.9 | -5 % (better) |
| session50 | fzstd + TypeScript | 77.5 (80.5) | 32.9 | 40.2 | 56.1 | 12.3 | +7 % (worse) |

Block: the middle one. cross-origin isolated: true.

Bytes fetched a recording, head then the middle block: gcc-swd-reads 1874 + 70825; stock-reads 14958 + 33308; swo-24000000 9672 + 126783; gcc-oscan1-probe-rs 19420 + 36536; gcc-swd-matrix-capture100000 53722 + 79047; session50 22085 + 265181. Machine busy during the runs (this run included): median 8 %, max 38 %.

### First inset, firefox 159.0, unthrottled

Milliseconds from navigation start to the drawn inset, median (p95) of 20 cold runs. A-A floor 2.6 %; "vs ruzstd" compares medians with the decoder-only build.

| recording | decoder | drawn | worker up | decoder ready | block fetched | decode | vs ruzstd |
| --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, default build (ruzstd) | 254.4 (303.4) | 168.7 | 226.4 | 233.9 | 14.0 | same |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 254.8 (266.9) | 169.5 | 226.7 | 234.4 | 13.9 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 254.1 (273.4) | 168.8 | 224.1 | 232.6 | 15.1 |  |
| gcc-swd-reads | libzstd in WASM | 257.5 (270.1) | 173.6 | 228.9 | 237.0 | 13.3 | same |
| gcc-swd-reads | fzstd + TypeScript | 250.3 (264.2) | 168.9 | 171.0 | 227.4 | 16.3 | same |
| gcc-swd-reads | DecompressionStream("zstd") + TypeScript | 240.3 (253.6) | 170.2 | 170.6 | 226.4 | 7.2 | -6 % (better) |
| stock-reads | tickwave-wasm, default build (ruzstd) | 252.7 (271.3) | 170.8 | 230.3 | 237.4 | 9.8 | same |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 253.0 (264.1) | 169.7 | 228.1 | 237.3 | 9.9 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 255.6 (267.8) | 173.9 | 229.7 | 239.8 | 10.2 |  |
| stock-reads | libzstd in WASM | 250.6 (268.5) | 168.6 | 219.7 | 236.6 | 8.8 | same |
| stock-reads | fzstd + TypeScript | 239.2 (261.7) | 168.6 | 171.2 | 223.7 | 10.1 | -5 % (better) |
| stock-reads | DecompressionStream("zstd") + TypeScript | 243.7 (253.4) | 172.3 | 173.0 | 231.6 | 6.4 | -4 % (better) |
| swo-24000000 | tickwave-wasm, default build (ruzstd) | 257.4 (269.1) | 169.3 | 226.2 | 236.6 | 15.3 | same |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 256.7 (270.0) | 168.3 | 227.6 | 234.9 | 16.1 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 255.7 (267.6) | 168.6 | 226.0 | 233.3 | 16.0 |  |
| swo-24000000 | libzstd in WASM | 254.9 (269.2) | 169.2 | 228.3 | 235.2 | 13.3 | same |
| swo-24000000 | fzstd + TypeScript | 256.7 (269.1) | 172.6 | 175.1 | 236.0 | 15.2 | same |
| swo-24000000 | DecompressionStream("zstd") + TypeScript | 236.4 (252.4) | 167.0 | 167.4 | 223.1 | 6.9 | -8 % (better) |
| gcc-oscan1-probe-rs | tickwave-wasm, default build (ruzstd) | 251.9 (267.1) | 169.7 | 227.2 | 234.2 | 11.7 | same |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 248.2 (259.8) | 166.7 | 225.1 | 231.1 | 11.0 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 254.7 (267.4) | 169.9 | 225.5 | 237.9 | 10.9 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 256.8 (269.8) | 170.9 | 223.3 | 240.0 | 10.9 | +3 % (worse) |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 257.6 (267.6) | 170.5 | 172.8 | 235.1 | 16.3 | +4 % (worse) |
| gcc-oscan1-probe-rs | DecompressionStream("zstd") + TypeScript | 243.7 (254.3) | 170.1 | 170.5 | 231.2 | 6.4 | same |
| gcc-swd-matrix-capture100000 | tickwave-wasm, default build (ruzstd) | 262.4 (279.1) | 171.6 | 235.2 | 241.9 | 13.9 | same |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 257.6 (273.1) | 169.9 | 225.8 | 235.9 | 14.3 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 260.7 (268.8) | 170.0 | 231.4 | 240.1 | 14.2 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 259.3 (271.3) | 172.1 | 230.9 | 238.5 | 13.8 | same |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 257.4 (274.2) | 173.4 | 175.6 | 232.8 | 17.2 | same |
| gcc-swd-matrix-capture100000 | DecompressionStream("zstd") + TypeScript | 244.7 (255.9) | 168.3 | 168.7 | 231.6 | 6.3 | -5 % (better) |
| session50 | tickwave-wasm, default build (ruzstd) | 255.8 (269.8) | 170.0 | 231.1 | 238.4 | 10.1 | same |
| session50 | tickwave-wasm, decoder only (ruzstd) | 258.7 (270.0) | 173.0 | 233.1 | 240.2 | 10.6 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 255.1 (263.1) | 170.6 | 226.0 | 237.4 | 10.5 |  |
| session50 | libzstd in WASM | 248.0 (265.3) | 169.9 | 228.2 | 237.4 | 3.6 | -4 % (better) |
| session50 | fzstd + TypeScript | 258.3 (278.7) | 175.1 | 177.8 | 241.9 | 10.3 | same |
| session50 | DecompressionStream("zstd") + TypeScript | 244.7 (259.3) | 175.2 | 175.6 | 234.4 | 3.5 | -5 % (better) |

Block: the middle one. cross-origin isolated: true.

Bytes fetched a recording, head then the middle block: gcc-swd-reads 1874 + 70825; stock-reads 14958 + 33308; swo-24000000 9672 + 126783; gcc-oscan1-probe-rs 19420 + 36536; gcc-swd-matrix-capture100000 53722 + 79047; session50 22085 + 265181. Machine busy during the runs (this run included): median 5 %, max 36 %.

### First inset, chromium 158.0.8127.18, DevTools "Slow 4G"

Milliseconds from navigation start to the drawn inset, median (p95) of 20 cold runs. A-A floor 0.2 %; "vs ruzstd" compares medians with the decoder-only build.

| recording | decoder | drawn | worker up | decoder ready | block fetched | decode | vs ruzstd |
| --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, default build (ruzstd) | 4824.3 (4840.2) | 1760.3 | 3835.5 | 4800.6 | 14.7 | +13 % (worse) |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 4283.0 (4295.4) | 1760.8 | 3293.4 | 4259.2 | 14.2 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 4283.8 (4290.9) | 1760.5 | 3293.1 | 4259.1 | 14.7 |  |
| gcc-swd-reads | libzstd in WASM | 4419.5 (4428.0) | 1752.1 | 3426.8 | 4392.6 | 16.4 | +3 % (worse) |
| gcc-swd-reads | fzstd + TypeScript | 3731.8 (3755.0) | 1803.3 | 1806.5 | 3701.0 | 23.0 | -13 % (better) |
| stock-reads | tickwave-wasm, default build (ruzstd) | 4618.1 (4753.8) | 1760.4 | 3835.9 | 4600.6 | 10.4 | +13 % (worse) |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 4069.7 (4116.1) | 1760.5 | 3293.6 | 4051.3 | 10.7 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 4075.9 (4085.6) | 1760.6 | 3293.4 | 4059.0 | 9.9 |  |
| stock-reads | libzstd in WASM | 4212.8 (4221.8) | 1752.3 | 3427.1 | 4192.4 | 12.8 | +4 % (worse) |
| stock-reads | fzstd + TypeScript | 3511.3 (3519.4) | 1803.1 | 1806.1 | 3492.3 | 13.0 | -14 % (better) |
| swo-24000000 | tickwave-wasm, default build (ruzstd) | 5134.0 (5150.4) | 1760.3 | 3835.9 | 5109.1 | 16.9 | +12 % (worse) |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 4592.3 (4613.7) | 1760.4 | 3293.5 | 4567.8 | 17.1 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 4592.2 (4622.9) | 1760.6 | 3293.6 | 4568.2 | 16.4 |  |
| swo-24000000 | libzstd in WASM | 4726.8 (4736.0) | 1751.9 | 3426.7 | 4701.3 | 17.7 | +3 % (worse) |
| swo-24000000 | fzstd + TypeScript | 4043.8 (4048.5) | 1803.2 | 1806.8 | 4009.7 | 27.2 | -12 % (better) |
| gcc-oscan1-probe-rs | tickwave-wasm, default build (ruzstd) | 4629.4 (4645.1) | 1760.8 | 3835.9 | 4610.0 | 11.8 | +13 % (worse) |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 4086.5 (4096.4) | 1760.1 | 3293.3 | 4067.3 | 11.6 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 4086.3 (4096.5) | 1760.4 | 3293.5 | 4067.2 | 12.0 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 4223.8 (4232.3) | 1751.9 | 3426.5 | 4201.0 | 14.7 | +3 % (worse) |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 3541.6 (3548.6) | 1802.9 | 1806.3 | 3508.9 | 25.7 | -13 % (better) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, default build (ruzstd) | 4866.0 (4890.0) | 1760.6 | 3835.4 | 4842.7 | 15.0 | +13 % (worse) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 4323.6 (4332.5) | 1760.6 | 3293.4 | 4301.2 | 14.4 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 4323.8 (4340.6) | 1760.6 | 3293.6 | 4301.3 | 14.7 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 4467.9 (4476.5) | 1752.5 | 3427.2 | 4442.3 | 16.8 | +3 % (worse) |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 3771.5 (3781.1) | 1802.7 | 1806.1 | 3742.4 | 22.4 | -13 % (better) |
| session50 | tickwave-wasm, default build (ruzstd) | 5912.5 (5920.0) | 1760.4 | 3835.5 | 5892.8 | 8.9 | +10 % (worse) |
| session50 | tickwave-wasm, decoder only (ruzstd) | 5372.0 (5380.1) | 1760.9 | 3293.9 | 5351.7 | 9.5 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 5372.3 (5581.2) | 1760.3 | 3293.4 | 5351.3 | 8.7 |  |
| session50 | libzstd in WASM | 5502.9 (5530.8) | 1752.1 | 3427.1 | 5484.5 | 6.2 | +2 % (worse) |
| session50 | fzstd + TypeScript | 4817.4 (4851.2) | 1803.2 | 1806.8 | 4792.9 | 14.7 | -10 % (better) |

Block: the middle one. cross-origin isolated: true.

Bytes fetched a recording, head then the middle block: gcc-swd-reads 1874 + 70825; stock-reads 14958 + 33308; swo-24000000 9672 + 126783; gcc-oscan1-probe-rs 19420 + 36536; gcc-swd-matrix-capture100000 53722 + 79047; session50 22085 + 265181. Machine busy during the runs (this run included): median 1 %, max 75 %.

### Block decode, chromium 158.0.8127.18

Millions of edges a second (session50: of samples), median (p95, slow side) over 20 runs of every block. "In worker": decode inside the worker, to typed arrays. "Round trip": the page's request to the arrays arriving back, the worker hop included. A-A floor 2.2 % in worker, 2.1 % round trip.

| recording | decoder | in worker | round trip | hop ms | in worker vs ruzstd |
| --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 155.3 (137.6) | 151.6 (129.3) | 0.14 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 158.4 (139.3) | 153.9 (132.0) | 0.14 |  |
| gcc-swd-reads | libzstd in WASM | 139.7 (124.5) | 137.1 (120.8) | 0.14 | -10 % (worse) |
| gcc-swd-reads | fzstd + TypeScript | 101.6 (90.3) | 100.3 (89.4) | 0.11 | -35 % (worse) |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 121.5 (96.4) | 119.3 (91.1) | 0.10 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 118.8 (96.2) | 117.1 (93.0) | 0.10 |  |
| stock-reads | libzstd in WASM | 118.7 (96.5) | 116.3 (93.2) | 0.10 | -2 % (worse) |
| stock-reads | fzstd + TypeScript | 86.1 (60.1) | 83.9 (59.3) | 0.12 | -29 % (worse) |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 110.4 (101.2) | 108.8 (98.8) | 0.12 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 110.6 (100.9) | 109.0 (98.9) | 0.12 |  |
| swo-24000000 | libzstd in WASM | 116.1 (105.5) | 114.3 (103.0) | 0.12 | +5 % (better) |
| swo-24000000 | fzstd + TypeScript | 98.5 (91.2) | 97.3 (89.7) | 0.10 | -11 % (worse) |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 157.5 (121.0) | 153.7 (114.8) | 0.11 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 156.7 (116.8) | 152.8 (111.3) | 0.11 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 138.2 (111.8) | 135.4 (106.8) | 0.11 | -12 % (worse) |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 116.2 (81.2) | 112.4 (76.1) | 0.09 | -26 % (worse) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 145.5 (104.6) | 142.4 (100.0) | 0.11 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 142.2 (100.2) | 139.4 (97.0) | 0.11 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 133.1 (106.8) | 130.8 (102.5) | 0.11 | -8 % (worse) |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 107.4 (62.0) | 103.6 (57.3) | 0.15 | -26 % (worse) |
| session50 | tickwave-wasm, decoder only (ruzstd) | 89.6 (73.5) | 88.2 (71.4) | 0.09 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 88.6 (55.8) | 87.1 (53.0) | 0.09 |  |
| session50 | libzstd in WASM | 346.1 (182.7) | 329.7 (173.0) | 0.07 | +286 % (better) |
| session50 | fzstd + TypeScript | 70.9 (59.9) | 70.0 (57.6) | 0.09 | -21 % (worse) |

Machine busy during the runs (this run included): median 4 %, max 75 %.

Median ms a block by step, every recording pooled:

| decoder | steps |
| --- | --- |
| tickwave-wasm, decoder only (ruzstd) | decode 4.64, copyOut 1.34 |
| libzstd in WASM | decode 4.68, copyOut 1.34 |
| fzstd + TypeScript | inflate 2.84, checksum 0.09, parse 4.70 |

### Block decode, firefox 159.0

Millions of edges a second (session50: of samples), median (p95, slow side) over 20 runs of every block. "In worker": decode inside the worker, to typed arrays. "Round trip": the page's request to the arrays arriving back, the worker hop included. A-A floor 1.5 % in worker, 1.7 % round trip.

| recording | decoder | in worker | round trip | hop ms | in worker vs ruzstd |
| --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 137.9 (119.1) | 133.8 (114.0) | 0.22 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 138.8 (123.5) | 135.3 (103.9) | 0.20 |  |
| gcc-swd-reads | libzstd in WASM | 153.7 (138.2) | 148.8 (132.1) | 0.22 | +12 % (better) |
| gcc-swd-reads | fzstd + TypeScript | 168.2 (127.8) | 162.2 (114.2) | 0.16 | +22 % (better) |
| gcc-swd-reads | DecompressionStream("zstd") + TypeScript | 205.6 (174.0) | 196.0 (139.7) | 0.18 | +49 % (better) |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 107.2 (84.4) | 103.9 (81.3) | 0.14 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 105.6 (83.8) | 102.2 (80.2) | 0.16 |  |
| stock-reads | libzstd in WASM | 131.5 (106.4) | 126.3 (99.8) | 0.14 | +23 % (better) |
| stock-reads | fzstd + TypeScript | 119.3 (91.9) | 114.6 (83.1) | 0.14 | +11 % (better) |
| stock-reads | DecompressionStream("zstd") + TypeScript | 149.7 (114.1) | 142.1 (104.3) | 0.14 | +40 % (better) |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 86.4 (77.4) | 84.9 (74.7) | 0.16 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 86.3 (77.4) | 84.6 (74.1) | 0.18 |  |
| swo-24000000 | libzstd in WASM | 129.2 (113.7) | 126.1 (108.1) | 0.16 | +50 % (better) |
| swo-24000000 | fzstd + TypeScript | 120.4 (87.2) | 118.0 (82.7) | 0.14 | +39 % (better) |
| swo-24000000 | DecompressionStream("zstd") + TypeScript | 168.2 (142.5) | 162.1 (122.5) | 0.14 | +95 % (better) |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 137.0 (108.6) | 132.6 (104.7) | 0.16 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 136.5 (107.9) | 132.2 (103.4) | 0.16 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 149.6 (125.1) | 144.9 (117.9) | 0.14 | +9 % (better) |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 129.8 (89.3) | 125.7 (82.5) | 0.14 | -5 % (worse) |
| gcc-oscan1-probe-rs | DecompressionStream("zstd") + TypeScript | 166.1 (131.7) | 159.6 (118.4) | 0.12 | +21 % (better) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 122.2 (94.3) | 119.1 (91.0) | 0.16 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 121.9 (95.2) | 119.0 (92.6) | 0.16 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 148.8 (117.4) | 144.8 (112.4) | 0.14 | +22 % (better) |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 138.9 (100.5) | 135.3 (93.6) | 0.12 | +14 % (better) |
| gcc-swd-matrix-capture100000 | DecompressionStream("zstd") + TypeScript | 168.4 (128.1) | 163.7 (117.4) | 0.14 | +38 % (better) |
| session50 | tickwave-wasm, decoder only (ruzstd) | 71.2 (58.4) | 69.9 (56.9) | 0.12 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 71.2 (58.1) | 69.9 (56.4) | 0.12 |  |
| session50 | libzstd in WASM | 315.8 (154.2) | 297.9 (148.1) | 0.10 | +343 % (better) |
| session50 | fzstd + TypeScript | 62.9 (54.2) | 61.8 (51.5) | 0.14 | -12 % (worse) |
| session50 | DecompressionStream("zstd") + TypeScript | 238.3 (161.8) | 226.0 (145.6) | 0.12 | +235 % (better) |

Machine busy during the runs (this run included): median 4 %, max 5 %.

Median ms a block by step, every recording pooled:

| decoder | steps |
| --- | --- |
| tickwave-wasm, decoder only (ruzstd) | decode 5.86, copyOut 1.14 |
| libzstd in WASM | decode 4.34, copyOut 1.04 |
| fzstd + TypeScript | inflate 2.74, checksum 0.10, parse 3.46 |
| DecompressionStream("zstd") + TypeScript | inflate 1.30, parse 3.42 |

### Checksum (XXH64 over the decoded block), chromium

stock-reads, 20 runs of each of its 1 MiB blocks (the last is shorter).

| decoder | checksum ms median | p95 | whole block ms median |
| --- | --- | --- | --- |
| fzstd + TypeScript | 0.09 | 0.11 | 7.21 |
| fzstd + TypeScript, pure-JS XXH64 | 19.95 | 25.22 | 27.58 |

### Checksum (XXH64 over the decoded block), firefox

stock-reads, 20 runs of each of its 1 MiB blocks (the last is shorter).

| decoder | checksum ms median | p95 | whole block ms median |
| --- | --- | --- | --- |
| fzstd + TypeScript | 0.10 | 0.12 | 5.48 |
| fzstd + TypeScript, pure-JS XXH64 | 21.88 | 22.42 | 27.24 |

### The largest recording decoded whole (gcc-swd-matrix-capture100000, 742 blocks, 637,277,314 edges)

Seconds, median (p95) of the runs, every block in turn inside one worker; M edges a second at the median; the machine's median busy fraction during the runs, this run included.

| browser | decoder | seconds | M edges/s | vs ruzstd | machine busy |
| --- | --- | --- | --- | --- | --- |
| chromium 158.0.8127.18 | tickwave-wasm, decoder only (ruzstd) | 4.42 (4.47) | 144 | A-A 0.3 % | 5 % |
| chromium 158.0.8127.18 | tickwave-wasm, decoder only (ruzstd), A-A copy | 4.44 (4.49) | 144 | A-A 0.3 % | 4 % |
| chromium 158.0.8127.18 | libzstd in WASM | 4.77 (4.83) | 134 | +8 % (worse) | 5 % |
| chromium 158.0.8127.18 | fzstd + TypeScript | 6.75 (6.84) | 94 | +53 % (worse) | 6 % |
| firefox 159.0 | tickwave-wasm, decoder only (ruzstd) | 5.13 (5.15) | 124 | A-A 0.2 % | 4 % |
| firefox 159.0 | tickwave-wasm, decoder only (ruzstd), A-A copy | 5.12 (5.15) | 124 | A-A 0.2 % | 4 % |
| firefox 159.0 | libzstd in WASM | 4.29 (4.33) | 149 | -16 % (better) | 4 % |
| firefox 159.0 | fzstd + TypeScript | 4.73 (4.78) | 135 | -8 % (better) | 4 % |
| firefox 159.0 | DecompressionStream("zstd") + TypeScript | 3.96 (3.99) | 161 | -23 % (better) | 4 % |
| native | Head::decode_block (ruzstd), one thread | 2.36 (2.36) | 271 |  |  |

### Native reference, Head::decode_block

M items a second, median (p95, slow side) over 20 runs of every block.

| recording | native |
| --- | --- |
| gcc-swd-reads | 285.1 (254.0) |
| stock-reads | 225.1 (189.5) |
| swo-24000000 | 171.7 (159.9) |
| gcc-oscan1-probe-rs | 326.3 (257.5) |
| gcc-swd-matrix-capture100000 | 275.2 (194.2) |
| session50 | 127.1 (98.6) |

## Copies and the hop to the worker

Per block, as the code does it (read in the wasm-bindgen glue, tickwave-wasm and the TypeScript):

| Decoder | Into WASM memory | Inside | Out of WASM memory | Hop to the page |
| --- | --- | --- | --- | --- |
| tickwave-wasm, libzstd | the compressed frame (`&[u8]`: malloc and copy) | inflate to a 1 MiB payload; parse to `Vec<i64>` edges; `edges()` maps them to a new `Vec<f64>` | each channel's `Vec<f64>` or `Vec<f32>` copied to a new JS array (`Float64Array::from`) | the arrays transferred, no copy |
| fzstd + TypeScript | the 1 MiB payload, copied into hash-wasm's memory for XXH64 | n/a | n/a | the same |
| DecompressionStream + TypeScript | none | n/a | n/a | the same |

The JavaScript readers write edges straight into the `Float64Array` they return. The stream's
output chunks are copied once into one payload buffer. The request to the worker is a few bytes;
in the block measure the worker already holds the file.

What they cost, from the step tables: getting a block's arrays out of WASM memory (the i64-to-f64
pass, the copy out, and freeing the block) takes a median 1.34 ms in Chromium and 1.14 ms in
Firefox, 22 % and 16 % of tickwave-wasm's time a block. The hop to the page and back takes a median
0.07-0.15 ms in Chromium and 0.10-0.22 ms in Firefox.

## Correctness

Every block of every recording went through every decoder in both browsers. Each block's output
was hashed (SHA-256 over the start tick, each channel's level, edges as i64 and values' f32
bits) and compared with `Head::decode_block` natively. Every overview block went the same way,
against `Head::decode_overview_block`, in every decoder that can decode one. The TypeScript
reader shares no code with the Rust, so its agreement checks both.

The corrupted blocks, all of the middle block of gcc-swd-reads (block 3) and session50 (block
191), made by `tickwave-bench-native corrupt`:

| Kind | What was done |
| --- | --- |
| flip-data | one bit flipped in the middle of the frame |
| flip-checksum | one bit flipped in the stored checksum |
| no-checksum | a valid frame of the same length that declares no checksum (its size field widened to fill the 4 bytes) |
| zero-interval, section-shift | the payload changed (two edges on one tick; 4 bytes moved between two section lengths) and recompressed with a good checksum, so only the block parser can tell |
| trailing-frame | an empty skippable frame after the frame |
| trailing-empty-frame | an empty zstd frame, with its checksum, after the frame |
| flip-data-then-empty-frame | flip-data's frame, then an empty zstd frame |
| trailing-frame-checksum | a skippable frame holding the block's own checksum after the frame, so the last 4 bytes still match |
| decoded-longer, decoded-shorter | the index's decoded length 1 more or 1 less than the frame holds |

The frame kinds are decoded against the file's own head; the others are whole files, with the
index and preamble moved to fit. Each decoder also decodes an untouched block of the same file
first, so a refusal is not a broken setup.

### Correctness, chromium 158.0.8127.18

DecompressionStream("zstd"): no by default. Cross-origin isolated: true.

| decoder | blocks hashed | differ from native | overview blocks hashed | differ from native |
| --- | --- | --- | --- | --- |
| tickwave-wasm, default build (ruzstd) | 1505 | 0 | 610 | 0 |
| tickwave-wasm, decoder only (ruzstd) | 1505 | 0 | 610 | 0 |
| libzstd in WASM | 1505 | 0 | no API |  |
| fzstd + TypeScript | 1505 | 0 | 610 | 0 |
| fzstd + TypeScript, pure-JS XXH64 | 1505 | 0 | 610 | 0 |

| recording | corruption | native refusal | tickwave-wasm, default build (ruzstd) | tickwave-wasm, decoder only (ruzstd) | libzstd in WASM | fzstd + TypeScript | fzstd + TypeScript, pure-JS XXH64 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | flip-data | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum a841ba4d does not match c8efab53 | refused: Error: zstd content checksum a841ba4d does not match c8efab53 |
| gcc-swd-reads | flip-checksum | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum c8efab53 does not match c9efab53 | refused: Error: zstd content checksum c8efab53 does not match c9efab53 |
| gcc-swd-reads | no-checksum | a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: zstd frame has no content checksum | refused: Error: zstd frame has no content checksum |
| gcc-swd-reads | zero-interval | block 3, channel SWCLK: two edges share a tick | refused: Error: block 3, channel SWCLK: two edges share a tick | refused: Error: block 3, channel SWCLK: two edges share a tick | refused: Error: block 3, channel SWCLK: two edges share a tick | refused: Error: block 3 channel 0: a zero interval after the first | refused: Error: block 3 channel 0: a zero interval after the first |
| gcc-swd-reads | trailing-frame | accepted | accepted | accepted | refused: Error: 8 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 8 more | refused: Error: zstd frame is 70825 bytes, followed by 8 more |
| gcc-swd-reads | trailing-empty-frame | accepted | accepted | accepted | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 13 more | refused: Error: zstd frame is 70825 bytes, followed by 13 more |
| gcc-swd-reads | flip-data-then-empty-frame | block 3, channel SWCLK: bytes follow the last edge | refused: Error: block 3, channel SWCLK: bytes follow the last edge | refused: Error: block 3, channel SWCLK: bytes follow the last edge | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 13 more | refused: Error: zstd frame is 70825 bytes, followed by 13 more |
| gcc-swd-reads | trailing-frame-checksum | accepted | accepted | accepted | refused: Error: 12 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 12 more | refused: Error: zstd frame is 70825 bytes, followed by 12 more |
| gcc-swd-reads | decoded-longer | a frame decoded to 1048584 bytes, not 1048585 | refused: Error: a frame decoded to 1048584 bytes, not 1048585 | refused: Error: a frame decoded to 1048584 bytes, not 1048585 | refused: Error: a frame decoded to 1048584 bytes, not 1048585 | refused: Error: zstd frame decoded to 1048584 bytes, not 1048585 | refused: Error: zstd frame decoded to 1048584 bytes, not 1048585 |
| gcc-swd-reads | decoded-shorter | zstd: Target must have at least as many bytes as the contentsize of the frame reports | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Destination buffer is too small | refused: Error: zstd frame decoded to 1048584 bytes, not 1048583 | refused: Error: zstd frame decoded to 1048584 bytes, not 1048583 |
| session50 | flip-data | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum ffd8baa0 does not match 89b5a223 | refused: Error: zstd content checksum ffd8baa0 does not match 89b5a223 |
| session50 | flip-checksum | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum 89b5a223 does not match 88b5a223 | refused: Error: zstd content checksum 89b5a223 does not match 88b5a223 |
| session50 | no-checksum | a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: zstd frame has no content checksum | refused: Error: zstd frame has no content checksum |
| session50 | section-shift | block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191 channel 0: 524284 bytes for 262144 codes | refused: Error: block 191 channel 0: 524284 bytes for 262144 codes |
| session50 | trailing-frame | accepted | accepted | accepted | refused: Error: 8 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 8 more | refused: Error: zstd frame is 265181 bytes, followed by 8 more |
| session50 | trailing-empty-frame | accepted | accepted | accepted | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 13 more | refused: Error: zstd frame is 265181 bytes, followed by 13 more |
| session50 | flip-data-then-empty-frame | accepted | accepted | accepted | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 13 more | refused: Error: zstd frame is 265181 bytes, followed by 13 more |
| session50 | trailing-frame-checksum | accepted | accepted | accepted | refused: Error: 12 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 12 more | refused: Error: zstd frame is 265181 bytes, followed by 12 more |
| session50 | decoded-longer | a frame decoded to 1048588 bytes, not 1048589 | refused: Error: a frame decoded to 1048588 bytes, not 1048589 | refused: Error: a frame decoded to 1048588 bytes, not 1048589 | refused: Error: a frame decoded to 1048588 bytes, not 1048589 | refused: Error: zstd frame decoded to 1048588 bytes, not 1048589 | refused: Error: zstd frame decoded to 1048588 bytes, not 1048589 |
| session50 | decoded-shorter | zstd: Target must have at least as many bytes as the contentsize of the frame reports | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Destination buffer is too small | refused: Error: zstd frame decoded to 1048588 bytes, not 1048587 | refused: Error: zstd frame decoded to 1048588 bytes, not 1048587 |

### Correctness, firefox 159.0

DecompressionStream("zstd"): no by default, yes with dom.compression_streams.zstd.enabled. Cross-origin isolated: true.

| decoder | blocks hashed | differ from native | overview blocks hashed | differ from native |
| --- | --- | --- | --- | --- |
| tickwave-wasm, default build (ruzstd) | 1505 | 0 | 610 | 0 |
| tickwave-wasm, decoder only (ruzstd) | 1505 | 0 | 610 | 0 |
| libzstd in WASM | 1505 | 0 | no API |  |
| fzstd + TypeScript | 1505 | 0 | 610 | 0 |
| fzstd + TypeScript, pure-JS XXH64 | 1505 | 0 | 610 | 0 |
| DecompressionStream("zstd") + TypeScript | 1505 | 0 | 610 | 0 |

| recording | corruption | native refusal | tickwave-wasm, default build (ruzstd) | tickwave-wasm, decoder only (ruzstd) | libzstd in WASM | fzstd + TypeScript | fzstd + TypeScript, pure-JS XXH64 | DecompressionStream("zstd") + TypeScript |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | flip-data | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum a841ba4d does not match c8efab53 | refused: Error: zstd content checksum a841ba4d does not match c8efab53 | refused: TypeError: zstd decompression error: Restored data doesn't match checksum |
| gcc-swd-reads | flip-checksum | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum c8efab53 does not match c9efab53 | refused: Error: zstd content checksum c8efab53 does not match c9efab53 | refused: TypeError: zstd decompression error: Restored data doesn't match checksum |
| gcc-swd-reads | no-checksum | a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: zstd frame has no content checksum | refused: Error: zstd frame has no content checksum | refused: Error: zstd frame has no content checksum |
| gcc-swd-reads | zero-interval | block 3, channel SWCLK: two edges share a tick | refused: Error: block 3, channel SWCLK: two edges share a tick | refused: Error: block 3, channel SWCLK: two edges share a tick | refused: Error: block 3, channel SWCLK: two edges share a tick | refused: Error: block 3 channel 0: a zero interval after the first | refused: Error: block 3 channel 0: a zero interval after the first | refused: Error: block 3 channel 0: a zero interval after the first |
| gcc-swd-reads | trailing-frame | accepted | accepted | accepted | refused: Error: 8 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 8 more | refused: Error: zstd frame is 70825 bytes, followed by 8 more | refused: Error: zstd frame is 70825 bytes, followed by 8 more |
| gcc-swd-reads | trailing-empty-frame | accepted | accepted | accepted | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 13 more | refused: Error: zstd frame is 70825 bytes, followed by 13 more | refused: Error: zstd frame is 70825 bytes, followed by 13 more |
| gcc-swd-reads | flip-data-then-empty-frame | block 3, channel SWCLK: bytes follow the last edge | refused: Error: block 3, channel SWCLK: bytes follow the last edge | refused: Error: block 3, channel SWCLK: bytes follow the last edge | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 13 more | refused: Error: zstd frame is 70825 bytes, followed by 13 more | refused: Error: zstd frame is 70825 bytes, followed by 13 more |
| gcc-swd-reads | trailing-frame-checksum | accepted | accepted | accepted | refused: Error: 12 bytes follow the zstd frame | refused: Error: zstd frame is 70825 bytes, followed by 12 more | refused: Error: zstd frame is 70825 bytes, followed by 12 more | refused: Error: zstd frame is 70825 bytes, followed by 12 more |
| gcc-swd-reads | decoded-longer | a frame decoded to 1048584 bytes, not 1048585 | refused: Error: a frame decoded to 1048584 bytes, not 1048585 | refused: Error: a frame decoded to 1048584 bytes, not 1048585 | refused: Error: a frame decoded to 1048584 bytes, not 1048585 | refused: Error: zstd frame decoded to 1048584 bytes, not 1048585 | refused: Error: zstd frame decoded to 1048584 bytes, not 1048585 | refused: Error: zstd frame decoded to 1048584 bytes, not 1048585 |
| gcc-swd-reads | decoded-shorter | zstd: Target must have at least as many bytes as the contentsize of the frame reports | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Destination buffer is too small | refused: Error: zstd frame decoded to 1048584 bytes, not 1048583 | refused: Error: zstd frame decoded to 1048584 bytes, not 1048583 | refused: Error: zstd frame decodes past 1048583 bytes |
| session50 | flip-data | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum ffd8baa0 does not match 89b5a223 | refused: Error: zstd content checksum ffd8baa0 does not match 89b5a223 | refused: TypeError: zstd decompression error: Restored data doesn't match checksum |
| session50 | flip-checksum | zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: checksum mismatch | refused: Error: zstd: Restored data doesn't match checksum | refused: Error: zstd content checksum 89b5a223 does not match 88b5a223 | refused: Error: zstd content checksum 89b5a223 does not match 88b5a223 | refused: TypeError: zstd decompression error: Restored data doesn't match checksum |
| session50 | no-checksum | a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: a frame carries no checksum | refused: Error: zstd frame has no content checksum | refused: Error: zstd frame has no content checksum | refused: Error: zstd frame has no content checksum |
| session50 | section-shift | block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191, channel SWCLK: the section does not hold its samples | refused: Error: block 191 channel 0: 524284 bytes for 262144 codes | refused: Error: block 191 channel 0: 524284 bytes for 262144 codes | refused: Error: block 191 channel 0: 524284 bytes for 262144 codes |
| session50 | trailing-frame | accepted | accepted | accepted | refused: Error: 8 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 8 more | refused: Error: zstd frame is 265181 bytes, followed by 8 more | refused: Error: zstd frame is 265181 bytes, followed by 8 more |
| session50 | trailing-empty-frame | accepted | accepted | accepted | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 13 more | refused: Error: zstd frame is 265181 bytes, followed by 13 more | refused: Error: zstd frame is 265181 bytes, followed by 13 more |
| session50 | flip-data-then-empty-frame | accepted | accepted | accepted | refused: Error: 13 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 13 more | refused: Error: zstd frame is 265181 bytes, followed by 13 more | refused: Error: zstd frame is 265181 bytes, followed by 13 more |
| session50 | trailing-frame-checksum | accepted | accepted | accepted | refused: Error: 12 bytes follow the zstd frame | refused: Error: zstd frame is 265181 bytes, followed by 12 more | refused: Error: zstd frame is 265181 bytes, followed by 12 more | refused: Error: zstd frame is 265181 bytes, followed by 12 more |
| session50 | decoded-longer | a frame decoded to 1048588 bytes, not 1048589 | refused: Error: a frame decoded to 1048588 bytes, not 1048589 | refused: Error: a frame decoded to 1048588 bytes, not 1048589 | refused: Error: a frame decoded to 1048588 bytes, not 1048589 | refused: Error: zstd frame decoded to 1048588 bytes, not 1048589 | refused: Error: zstd frame decoded to 1048588 bytes, not 1048589 | refused: Error: zstd frame decoded to 1048588 bytes, not 1048589 |
| session50 | decoded-shorter | zstd: Target must have at least as many bytes as the contentsize of the frame reports | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Target must have at least as many bytes as the contentsize of the frame repor | refused: Error: zstd: Destination buffer is too small | refused: Error: zstd frame decoded to 1048588 bytes, not 1048587 | refused: Error: zstd frame decoded to 1048588 bytes, not 1048587 | refused: Error: zstd frame decodes past 1048587 bytes |

### A defect in the native decoder: one frame a block is not enforced

Fixed after the run, in "format: refuse anything but one checksummed frame a block": `inflate`
refuses unless the last frame decoded spans every byte it was given. The native decoder from
`main` before the fix accepts 7 of the 14 corrupted copies in `build/corrupt/` (all six trailing
kinds and session50's flip-data-then-empty-frame); with the fix it refuses all 14. The browser
runs below are from before the fix.

`crates/format/src/decode.rs`, `inflate` (lines 39-60 at `48d7f58`):

```rust
    let mut decoder = FrameDecoder::new();
    decoder
        .decode_all_to_vec(frame, &mut out)
        .map_err(|e| DecodeError::Zstd(e.to_string()))?;
    match (
        decoder.get_checksum_from_data(),
        decoder.get_calculated_checksum(),
    ) {
```

ruzstd 0.9.1's `decode_all` (`src/decoding/frame_decoder.rs`, lines 541-576) decodes every frame
in its input, `while !input.is_empty()`, and steps over skippable frames. It checks no checksum
itself. The two getters read the state of the last frame decoded. So `inflate` checks only the
last frame's checksum, and accepts bytes after the block's frame. FORMAT.md says a block is one
zstd frame.

What that lets through, natively and in both tickwave-wasm builds in both browsers:

- A block's frame followed by a skippable frame, empty or not, or by an empty zstd frame:
  accepted.
- session50's block 191 with one bit flipped in its frame (which alone fails "checksum
  mismatch"), followed by an empty zstd frame: accepted, and it decodes to wrong values. Its hash
  differs from the reference's, and no error is raised.

The libzstd build and the JavaScript readers refuse all of these. Closing it in `inflate`: refuse
any byte after the first frame. Decoding the frame alone and then checking that it consumed the
whole input would do it.

## Break tests

Each check was broken once on purpose and seen to fail, then restored from a saved copy. Unless
stated, in Chromium on identify-swd and gcc-swd-reads, with every corruption of both files. All
but the overview-block row ran before the reconversion, on the files and decoder tree of that
time; the checks they break did not change with it.

| Mutant | What failed |
| --- | --- |
| TypeScript reader: the eighth edge of every channel one tick late | every block in both fzstd decoders differs from native (8 of 8); every WASM block still matches |
| WASM path: the block's start level inverted | every block in tickwave-wasm (both) and libzstd differs (8 of 8) |
| One reference hash changed (gcc-swd-reads block 2) | block 2, and only block 2, differs in all five decoders |
| JavaScript checksum compare disabled | fzstd accepts flip-checksum (both files) and session50's flip-data; gcc-swd-reads' flip-data is still caught, by the reader |
| JavaScript check that a checksum is declared, removed (Firefox) | `DecompressionStream` accepts the frame without a checksum; fzstd still refuses it, because the check reads compressed bytes as the checksum |
| libzstd build: the declared-checksum check removed | libzstd accepts the frame without a checksum (both files) |
| libzstd build: the one-frame check removed | libzstd accepts trailing-frame and trailing-empty-frame (trailing-frame-checksum came later); it still refuses a bad frame followed by a good one, because libzstd checks every frame's checksum |
| JavaScript frame-end check removed (both browsers) | fzstd accepts a trailing skippable frame that ends in the block's own checksum. Every other trailing kind was still refused: Firefox's stream refuses trailing input itself, and fzstd's check reads the trailing bytes as the checksum |
| TypeScript zero-interval check removed | both fzstd decoders accept the block with two edges on one tick |
| TypeScript overview blocks: a digital channel's level and change bits swapped | all 4 of gcc-swd-reads' overview blocks differ in both fzstd decoders; the WASM ones still match |
| TypeScript codes read interleaved instead of in byte planes (session50) | caught first by the 12-bit range check (code 21333); with that check also removed, all 382 blocks differ in both fzstd decoders |
| Native tool: the no-checksum frame's descriptor left saying 4 size bytes while 8 are written | `corrupt` stops: libzstd's "Src size is incorrect" |
| The quiet gate, with a limit of 0 | waits out its 5 s limit and says "machine still busy (1 %)"; with a limit above 1 it returns at once |

The A-A pair has no break test: it is a measurement, not a check.

## What was not measured

- Overview blocks' decode speed. They were hash-checked, not timed.
- A real network or a CDN. The server is local, HTTP/1.1, gzip -9; GitHub Pages' own gzip level
  is unknown.
- Throttling in Firefox, which Playwright cannot do, and any profile but Slow 4G.
- A slower CPU. DevTools' CPU slowdown was not used. A mid-range phone's CPU would stretch decode
  and compile time, which could move the inset comparison either way.
- WebKit, any mobile browser, and Chromium's full build in its new headless mode: the runs use
  Playwright's headless shell.
- Memory: the WASM heap's growth, and the copies' peak.
- `DecompressionStream("zstd")` as readers would get it. No browser ships it on by default.
- Other builds: libzstd's `thin` feature (smaller, slower), wasm-opt `-Oz`, WASM SIMD, and a
  libzstd build without ruzstd. That one needs a `Head::parse` that takes the inflate, and overview
  blocks need a `decode_overview_block` that does.
- A pure-JavaScript XXH64 written for speed. The two published ones I found work in BigInt
  (`@intrnl/xxhash64`) or in `cuint` objects (`xxhashjs`). The checksum table has the BigInt one:
  about 20 ms a 1 MiB block, against 0.1 ms for hash-wasm's WASM one.
- A recording shorter than 64 KiB through the worker. Its first range fetch expects a full
  64 KiB, which every protocol recording has; a shorter file would be refused.
- The renderer. The stand-in draws one polyline; the viewer's drawing costs will differ.
- The f32 analog coding in the browsers' timings: session50 now holds codes. The f32 file it
  replaced did hash equal to native in every decoder in Chromium (763 blocks) before it was
  replaced.

## Where FORMAT.md fell short for an independent reader

All nine are settled in FORMAT.md since, in "format: refuse anything but one checksummed frame a
block", and the Rust refuses each case: an edge outside `1..=end`, a tick past `length`, block
starts that do not strictly increase, a code at or above `2^bits`, anything but one frame, a frame
naming a dictionary, and a head with no overview level. f32 is stated little-endian. The Rust now
also checks every block's level bit against `initial` and the edge counts before it.

The TypeScript reader was written from FORMAT.md before reading the Rust. Where the two then
differed, FORMAT.md is silent or loose:

1. An edge at tick 0. "Only the first [interval] may be zero" allows a zero first interval in
   block 0, which puts an edge at tick 0. The Rust refuses any edge before tick 1 (`tick < 1`).
   FORMAT.md should say which.
2. Edges after a channel's `end`. The Rust refuses an edge past the channel's `end`, and accepts
   one at `end`. FORMAT.md calls `end` "the tick up to which the level is known" and says neither.
   The reader written from the spec bounded edges by `length`.
3. How large a tick can be. The header promises integers below 2^53, but nothing bounds a tick in
   a block except, by implication, `length`. A JavaScript reader needs every tick below 2^53.
   "Every tick is at most `length`" would settle it, with item 2.
4. Index order. "In time order": the Rust refuses two blocks with the same start. "Strictly
   increasing" would say so.
5. f32 byte order. "All integers are little-endian"; f32 values are not integers. Analog f32
   samples and the overview's minimums and maximums are little-endian in the Rust.
6. Codes wider than `bits`. FORMAT.md does not say what a code at or above 2^`bits` means. The
   Rust accepts it; the TypeScript reader refuses it.
7. One frame a block. "One zstd frame a block" does not say that nothing may follow it, or whether
   skippable frames may. The native decoder accepts bytes after the frame (see the defect above).
8. An empty overview list. The Rust refuses a header with no overview level. The level rule
   implies at least one; FORMAT.md could say so.
9. Frames that name a dictionary. No dictionary exists, so such a frame cannot be read. FORMAT.md
   could forbid a dictionary id.

Two observations rather than gaps: the reader checks that block 0's level bit equals the header's
`initial`, which the Rust does not; and the fixed 64 KiB first fetch costs about 930 ms on Slow 4G
(562.5 ms latency plus 364 ms of transfer), while the heads here are 1.8-46 KB.

## Follow-ups after the protocol (2026-11-09)

ROUND.md's "Benchmark agent, follow-ups after the protocol". The tables above are unchanged; this
section adds to them.

**Base.** `main` at `6b82dda`, plus the decoder fix staged in `crates/format` ("format: refuse
anything but one checksummed frame a block"). `build/staged.diff` holds `git diff --cached` of
`crates/format` and `crates/wasm` (SHA-256 `6e6ac4e7…`). The tree also had another session's
unstaged changes to `crates/format/src/annotations.rs` and its test, which `build/decoder.diff`
(`ab0746be…`) records. Only the `annotations` feature compiles that file, so of the modules here
only tickwave-wasm's default build, in the correctness rerun, carries it. The recordings are the
ones reconverted on 2026-11-08 at 23:50, checked unchanged at the start and end of every run.

**Load.** Other sessions were converting on every core. No timed measure started until the
1-minute load average had stayed below 2 for five minutes, and every timed unit still waited for
three quiet seconds, now with no time limit. Load averages (1, 5, 15 minutes) at each measure's
start and end:

| Measure | Start | End |
| --- | --- | --- |
| first inset, Chromium, unthrottled | 0.33 0.71 1.84 | 2.30 1.21 1.95 |
| first inset, Chromium, Slow 4G | 0.26 0.56 1.45 | 1.56 0.81 0.73 |
| block decode, Chromium | 0.16 0.44 0.59 | 1.20 1.07 0.88 |
| first inset, Firefox, unthrottled | 0.36 0.54 0.69 | 2.19 1.87 1.36 |
| block decode, Firefox | 0.22 0.81 1.03 | 1.30 1.24 1.15 |
| inflate and parse apart, Chromium | 0.51 0.78 0.97 | 1.46 1.03 1.04 |

The two ends above 2 follow runs that launch a fresh browser 378 times; the median busy fraction
during those runs was 8 % (Chromium) and 4 % (Firefox), most of it the run's own.

### 1. libzstd without ruzstd

`bench/libzstd` now inflates the head's overview through `Head::parse_with`, blocks through
`decode_block_with`, and overview blocks through `decode_overview_block_with`, all with libzstd.
ruzstd is no longer linked in. twiggy over the modules' name sections:

| Module | ruzstd functions | libzstd functions (`ZSTD_`, `HUF_`, `FSE_`) |
| --- | --- | --- |
| libzstd build | 0 | 47 |
| probe build with ruzstd (control) | 40 (56,947 bytes) | 0 |
| tickwave-wasm, decoder only | 60 (41,037 bytes) | 0 |

The libzstd build now decodes overview blocks too, and matches native on all 610 of them.

Transfer size, as served (zlib level 9):

| Decoder | Module | Worker | Gzipped total |
| --- | --- | --- | --- |
| tickwave-wasm, decoder only | 261,676 (109,287 gzipped) | 13,584 (4,676) | 113,963 |
| libzstd, without ruzstd | 292,779 (115,285) | 12,717 (4,520) | 119,805 |
| libzstd with ruzstd, as measured above | 347,192 (132,400) | 10,762 (4,089) | 136,489 |

The libzstd build is now 5,842 bytes more than the decoder-only build gzipped, down from 24,576.
On Slow 4G that is 28-39 ms later to the first inset, against 131-144 ms before; its decoder is
ready 33.6 ms after tickwave-wasm's (3,344 against 3,310 ms), close to what 5,842 bytes take at
180,000 bytes a second (32 ms). Unthrottled, the two are within the floor on 7 of 12 rows, and
libzstd is 2-10 % sooner on four of the other five (10 % on session50 in Chromium) and 3 %
later on gcc-oscan1-probe-rs in Chromium.

In block decode, libzstd is now faster than tickwave-wasm on every recording in both browsers:
7-31 % on the digital ones in Chromium and 12-41 % in Firefox. Linked with ruzstd, at `48d7f58`,
it had been 12 % slower to 5 % faster in Chromium. ruzstd leaving and the staged decoder checks
arriving both happened between those builds, and I have not separated the two.

First inset, Chromium, unthrottled: median ms from navigation start to the drawn inset (20 cold runs); A-A floor 1.5 %.

| recording | ruzstd | A-A copy | libzstd | libzstd vs ruzstd |
| --- | --- | --- | --- | --- |
| gcc-swd-reads | 71.2 | 72.0 | 72.3 | same |
| stock-reads | 67.2 | 66.2 | 67.2 | same |
| swo-24000000 | 71.3 | 72.1 | 69.7 | -1.6 ms (-2 %) |
| gcc-oscan1-probe-rs | 68.0 | 68.7 | 70.0 | +2.0 ms (3 %) |
| gcc-swd-matrix-capture100000 | 72.0 | 72.7 | 71.0 | same |
| session50 | 75.6 | 74.9 | 68.1 | -7.4 ms (-10 %) |

First inset, Firefox, unthrottled: median ms from navigation start to the drawn inset (20 cold runs); A-A floor 2.0 %.

| recording | ruzstd | A-A copy | libzstd | libzstd vs ruzstd |
| --- | --- | --- | --- | --- |
| gcc-swd-reads | 255.0 | 254.7 | 247.7 | -7.3 ms (-3 %) |
| stock-reads | 252.6 | 250.4 | 248.7 | same |
| swo-24000000 | 259.3 | 257.0 | 249.2 | -10.1 ms (-4 %) |
| gcc-oscan1-probe-rs | 253.4 | 255.2 | 249.1 | same |
| gcc-swd-matrix-capture100000 | 256.4 | 256.7 | 254.0 | same |
| session50 | 251.5 | 256.6 | 250.4 | same |

First inset, Chromium, DevTools "Slow 4G": median ms from navigation start to the drawn inset (20 cold runs); A-A floor 0.2 %.

| recording | ruzstd | A-A copy | libzstd | libzstd vs ruzstd |
| --- | --- | --- | --- | --- |
| gcc-swd-reads | 4298.9 | 4299.0 | 4333.3 | +34.4 ms (1 %) |
| stock-reads | 4093.3 | 4093.7 | 4125.5 | +32.2 ms (1 %) |
| swo-24000000 | 4609.0 | 4608.7 | 4646.1 | +37.1 ms (1 %) |
| gcc-oscan1-probe-rs | 4104.0 | 4103.7 | 4137.7 | +33.7 ms (1 %) |
| gcc-swd-matrix-capture100000 | 4340.6 | 4347.7 | 4380.0 | +39.4 ms (1 %) |
| session50 | 5388.5 | 5388.6 | 5416.4 | +27.9 ms (1 %) |

Block decode in the worker, Chromium: median M edges a second (session50: samples), p95 slow side in brackets, 20 runs of every block; A-A floor 2.9 %.

| recording | ruzstd | A-A copy | libzstd | libzstd vs ruzstd |
| --- | --- | --- | --- | --- |
| gcc-swd-reads | 156.8 (134.4) | 161.3 (145.2) | 172.9 (146.7) | +10 % |
| stock-reads | 123.0 (99.6) | 123.5 (95.3) | 144.1 (116.3) | +17 % |
| swo-24000000 | 113.2 (103.3) | 112.9 (102.9) | 147.7 (132.9) | +31 % |
| gcc-oscan1-probe-rs | 160.5 (120.0) | 160.7 (121.8) | 171.3 (134.4) | +7 % |
| gcc-swd-matrix-capture100000 | 145.6 (107.3) | 147.6 (107.6) | 170.7 (132.8) | +17 % |
| session50 | 85.1 (71.6) | 84.8 (71.1) | 290.5 (172.5) | +241 % |

Block decode in the worker, Firefox: median M edges a second (session50: samples), p95 slow side in brackets, 20 runs of every block; A-A floor 2.0 %.

| recording | ruzstd | A-A copy | libzstd | libzstd vs ruzstd |
| --- | --- | --- | --- | --- |
| gcc-swd-reads | 139.7 (125.6) | 140.9 (125.5) | 161.4 (145.0) | +16 % |
| stock-reads | 114.1 (91.0) | 111.9 (86.4) | 137.2 (109.6) | +20 % |
| swo-24000000 | 97.1 (87.9) | 96.8 (87.8) | 136.4 (121.5) | +41 % |
| gcc-oscan1-probe-rs | 141.0 (111.5) | 141.0 (111.5) | 157.2 (124.0) | +12 % |
| gcc-swd-matrix-capture100000 | 129.7 (97.4) | 129.5 (95.9) | 157.3 (116.6) | +21 % |
| session50 | 68.4 (56.6) | 68.6 (56.7) | 270.3 (148.1) | +295 % |

### 2. session50's gap, attributed

The probe builds (`bench/libzstd --features probe`) decode a block as two calls, inflate and then
parse, and time them apart. They are the same crate, built once with libzstd and once with
tickwave-format's ruzstd `inflate`. Chromium, median ms a block, 20 runs of every block:

| Recording | Decoder | Inflate | Parse | Copy out | Whole |
| --- | --- | --- | --- | --- | --- |
| session50 | tickwave-wasm, decoder only | (5.73 with the parse) | | 0.44 | 6.21 |
| session50 | probe, ruzstd | 4.97 | 0.78 | 0.44 | 6.24 |
| session50 | probe, libzstd | 0.64 | 0.78 | 0.43 | 1.89 |
| gcc-swd-reads | tickwave-wasm, decoder only | (4.79 with the parse) | | 1.51 | 6.38 |
| gcc-swd-reads | probe, ruzstd | 1.81 | 4.59 | 1.51 | 7.90 |
| gcc-swd-reads | probe, libzstd | 0.73 | 4.60 | 1.50 | 6.89 |

session50's gap is in zstd decompression. ruzstd takes 4.97 ms to inflate a block and libzstd
0.64 ms, 7.8 times less; the parse takes 0.78 ms in both. On session50 the ruzstd probe totals
6.24 ms against tickwave-wasm's 6.21, so the probe stands for tickwave-wasm there.

On gcc-swd-reads, a digital recording added for contrast, libzstd also inflates faster (0.73
against 1.81 ms), and the parse is again the same in both probes. But the ruzstd probe totals 7.90
ms against tickwave-wasm's 6.38: splitting the call costs about 1.5 ms a block on digital blocks
and nothing measurable on session50's. So the probes compare with each other, but their digital
parse time is not tickwave-wasm's. Each probe call also copies the frame into WASM memory, one copy
more than `decodeBlock`.

### 3. Correctness at the new base

Every block and overview block of every recording, through every decoder, in both browsers,
against `Head::decode_block` and `Head::decode_overview_block` built from the staged decoder:

| Browser | Decoders | Blocks each | Differ | Overview blocks each | Differ |
| --- | --- | --- | --- | --- | --- |
| Chromium 158.0.8127.18 | 7: both tickwave-wasm builds, libzstd, both probes, fzstd, fzstd with pure-JS XXH64 | 1,505 | 0 | 610 | 0 |
| Firefox 159.0 | 8: the same and DecompressionStream | 1,505 | 0 | 610 | 0 |

The corrupted blocks are the ten kinds above plus one for each rule FORMAT.md settled (31 in all:
16 on gcc-swd-reads, 15 on session50). Every decoder refused every one, 217 in Chromium and 248 in
Firefox, and refused it at the stage it targets. Both tickwave-wasm builds now refuse all four
trailing kinds ("the bytes hold more than one frame"). The new kinds, each breaking one rule and
keeping every length:

| Kind | File | What was done | Refuse at | Native refusal |
| --- | --- | --- | --- | --- |
| dictionary | both | the frame made to name dictionary 42 (flag and id byte), otherwise unchanged | block | Frame header specified dictionary id 0x2A that wasnt provided |
| no-overview | both | the header's overview list emptied with spaces | head | the header lists no overview level |
| level-bit | gcc-swd-reads | block 3's level bit for SWCLK flipped in the index | head | channel SWCLK: block 3 starts at the wrong level |
| length-before-end | gcc-swd-reads | the header's length 927888190 made 127888190 | head | channel SWCLK ends outside the capture |
| edge-after-end | gcc-swd-reads | SWCLK's end made one tick past the last block's start | block (6) | block 6, channel SWCLK: an edge lies outside the block |
| edge-at-tick-0 | gcc-swd-reads | block 0's first interval on SWCLK replaced by a zero of the same length; recompressed | block (0) | block 0, channel SWCLK: an edge lies outside the block |
| bits-17 | session50 | SWCLK's bits 12 made 17 | head | channel SWCLK's codes are 17 bits |
| period-zero | session50 | SWCLK's period numerator made 0 | head | channel SWCLK's period has a zero term |
| code-too-wide | session50 | block 191's first code on SWCLK given a high byte of 0xff; recompressed | block | code 65368 is wider than 12 bits |

A decoder's refusal is recorded with where it happened: opening the head, decoding a control block
the corruption does not touch, or decoding the target block. A refusal anywhere but the target
counts against it.

### Break tests for the reader's new checks

FORMAT.md's settled rules gave the TypeScript reader new checks. Each was removed in turn, and
the reader run through the new kinds (Chromium, fzstd), then restored from a saved copy:

| Check removed | What the reader did |
| --- | --- |
| level bits follow from `initial` and the edge counts | accepted level-bit |
| an edge at or before its channel's `end` | accepted edge-after-end |
| no edge at tick 0 | accepted edge-at-tick-0 |
| no dictionary | accepted dictionary (both files) |
| `bits` in 1..=16 | accepted bits-17 |
| no zero term in `period` | accepted period-zero |
| `end` within 0..=`length` | still refused length-before-end, at the head, by the overview-block count ("overview level 12: 4 blocks for 31223 buckets") |
| at least one level in the head | still refused no-overview, at the head, by the overview's decoded size ("zstd frame decoded to 7524 bytes, not 0") |
| libzstd build: no dictionary | libzstd refused dictionary itself ("zstd: Dictionary mismatch") |

The last three checks are backed by other checks, which refuse the same input for another
reason. The stage check failed once by accident: the first edge-after-end lowered `end` so far
that the control block refused too, and the run reported "refused at control, expected block".

The two gates were broken as well. With a limit of 0, the load-average gate was still waiting
after 25 s. With a limit of 100 and a 1 s hold, it returned at its first check, after 10 s.

### Correction: what the switch costs in crates/wasm (tickwave session, 2026-11-09)

The size comparison above sets the libzstd crate in `bench/libzstd` against tickwave-wasm, and the
two crates do not carry the same code: tickwave-wasm also carries about 16 KB of core's float
formatting, which the libzstd crate does not. The switch itself, measured within one crate: at
`6b82dda` plus the staged fix, `crates/wasm` built with `--no-default-features` and the same
`wasm-opt` flags is 261,676 bytes (108,914 gzipped) with ruzstd and 313,570 bytes (123,777
gzipped) with libzstd. That is 14.9 KB gzipped more, about 83 ms on Slow 4G at 180,000 B/s, not
5.8 KB and 32 ms. The decode speeds above are unaffected. Where the float formatting comes from is
left for the size pass.
