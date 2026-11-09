# tickwave decode benchmark: results

tickwave decodes its recordings in the browser with a Rust decoder compiled to WebAssembly (WASM).
This benchmark asks whether tickwave also needs a decoder written in JavaScript. It times both
kinds on six real recordings in Chromium and Firefox, and it checks every decoder's output
against the native Rust decoder. `README.md` says how to rerun every number here.

The first runs were made while the project was called chrono. This document uses the new name
throughout. Only the names changed, not the figures.

## What is compared

A tickwave recording starts with its head: a header, an index of the blocks, and a coarse overview
of every channel. The blocks follow. Each block is one zstd frame that decodes to about 1 MiB. A
viewer fetches only the blocks a view covers, by HTTP range request. The overview's finer levels
sit in overview blocks, fetched the same way. `FORMAT.md` specifies the format. Below, inflating a
block means decompressing its zstd frame.

The decoders:

- **tickwave-wasm** is tickwave's decoder (`crates/wasm`) compiled to WASM. Its default build also
  holds `resolveAnnotations`, which resolves and checks a recording's sidecar, the file of
  annotations kept beside it. The decoder-only build (`--no-default-features`) leaves that out.
  tickwave-wasm inflated blocks with ruzstd, a zstd decoder written in Rust, until 3d17dd1 switched
  it to libzstd, the reference zstd library in C, compiled to WASM.
- **libzstd in WASM** (`bench/libzstd`) is tickwave-wasm's decoder with libzstd inflating every
  block. The benchmark measured libzstd this way before tickwave-wasm took it up.
- **fzstd with the TypeScript reader.** fzstd is a zstd decoder written in JavaScript. The reader
  (`src/format.ts`) parses the format, and was written from `FORMAT.md` without reading the Rust.
  In the comparisons below, "fzstd" means this pair.
- **`DecompressionStream("zstd")`** is the browser's own zstd decoder, used with the same reader,
  in a browser that has one.

The measures:

- **First inset.** An inset is the small view of a recording that tickwave's pages show beside
  their text. The first-inset time runs from navigation start until a cold page has drawn one. In
  that time the worker fetches and compiles the decoder, fetches the head and one block, and
  decodes the block. The page then draws it. Measured unthrottled, and in Chromium also under
  DevTools' "Slow 4G" profile: 562.5 ms of latency, and 180 KB a second down.
- **Block decode.** Every block of each recording, decoded in a Web Worker as the viewer decodes
  them, in millions of edges a second (of samples, for the analog recording).
- **Whole file.** The largest recording, decoded block after block.
- **Correctness.** Every decoder's output, hashed and compared with the native decoder's, and
  corrupted blocks that every decoder must refuse.

The first-inset, block-decode and whole-file measures run the decoder-only tickwave-wasm build
twice, as two separate decoders: the A-A pair. A table's A-A floor is the largest relative gap
between the pair's medians over the table's recordings. The decoder-only build is the reference in
every comparison. A gap to it smaller than the floor counts as no difference, and the tables print
it as "same".

## What the numbers say

No bar was set in advance for a JavaScript decoder: the maintainer decides from these numbers. An
early draft of the project plan proposed one. A JavaScript decoder would earn its place if the
first inset arrived more than 50 ms sooner with it on the throttled profile, or if it decoded
blocks faster than WASM. That draft was not adopted. For reference only, fzstd would pass it on
the first inset: on Slow 4G its first inset arrives more than 50 ms sooner on all six recordings,
against both builds. On block decode in Firefox it would pass on four of six recordings against
the ruzstd build the protocol run measured, but on one, gcc-swd-reads, against the libzstd build
tickwave ships. In Chromium it would fail on all six.

Chromium's figures below come from a rerun at e4ffc99, and Firefox's from the protocol run at
48d7f58. "The runs" explains the difference.

- **First inset on Slow 4G.** In Chromium, the first inset arrives 524-540 ms sooner with fzstd
  than with tickwave-wasm's decoder-only build on all six recordings, and 759-772 ms sooner than
  with its default build. The A-A floor is 1 ms. The gap is the decoder's own download.
  tickwave-wasm is 108 KB gzipped in the decoder-only build and 151 KB in the default, against
  13 KB for fzstd's worker, on a link that carries 180 KB a second. fzstd's decoder is ready at
  1.8 s, and tickwave-wasm's at 3.1 s and 3.3 s. Only Chromium was throttled.
- **First inset, unthrottled.** In Chromium, fzstd is within the 2.2 % floor on stock-reads and
  5-18 % (4-12 ms) later on the other five. In Firefox, the two are within the 2.6 % floor on four
  recordings; fzstd is 5 % sooner on one and 4 % later on another. A fresh Firefox takes about
  47 ms over its first fetch, the head's, and tickwave-wasm compiles in that time.
- **Block decode.** In Chromium, tickwave-wasm decodes faster than fzstd on every recording. fzstd
  is 33-43 % slower on the digital recordings and 75 % slower on session50, the analog one. The
  A-A floor is 1.8 %. In Firefox, the protocol run's tickwave-wasm inflates with ruzstd. fzstd is
  faster than it on four of six recordings (11-39 %) and slower on two: gcc-oscan1-probe-rs by 5 %
  and session50 by 12 %. The A-A floor is 1.5 %. In the same Firefox run, the libzstd build is
  7-15 % faster than fzstd on four digital recordings and 5.0 times as fast on session50; fzstd is
  9 % faster on gcc-swd-reads.
- **Whole file.** In the protocol run, the largest recording takes 4.42 s to decode with
  tickwave-wasm and 6.75 s with fzstd in Chromium, and 5.13 s and 4.73 s in Firefox. The libzstd
  build takes 4.77 s in Chromium and 4.29 s in Firefox.

Beside the bar:

- **libzstd.** In the protocol run, libzstd in WASM decoded session50's analog codes 4.4 times as
  fast as ruzstd in Firefox, and the digital recordings 9-50 % faster. Its module was 136 KB
  gzipped, 24 KB more than the decoder-only build, because it carried ruzstd as well, for the head.
  The follow-ups took ruzstd out of it. tickwave-wasm then switched to libzstd. In the Chromium
  rerun, the libzstd build and tickwave-wasm are within 4 % of each other in every timed measure.
- **The default build's sidecar code.** In the Chromium rerun, with the TOML parser gone from the
  module, it costs 233-235 ms on Slow 4G and is within the floor on all six unthrottled rows.
- **`DecompressionStream("zstd")`** decodes fastest in Firefox on five of six recordings, but only
  behind a pref. Chromium does not have it.
- **The worker hop** costs 0.1-0.2 ms a block. Decoding a block takes 5-8 ms, the medians of its
  steps summed.
- **Correctness.** In both browsers in the protocol run, and in the Chromium rerun, every decoder
  matches the native decoder on all 1,505 blocks. Every decoder that can decode overview blocks
  matches it on all 610 of them. In the protocol run, the native decoder accepted bytes after a
  block's frame, and with them a corrupted block. tickwave-wasm inherited that. 3bf88fb fixed
  it, and in the Chromium rerun every decoder refuses all 31 corrupted blocks. "A defect in the
  native decoder" below has the details.

## The runs

- **The protocol run**, at tickwave 48d7f58, in both browsers, started 2026-11-09 at 00:06 UTC.
  Its method was fixed before it ran. "How it was measured" states it. The decoders were built
  with no uncommitted changes to `crates/format` or `crates/wasm`. The recordings were the ones
  converted again, with overview blocks, on 2026-11-08 at 23:50.
- **The follow-ups**, at 6b82dda with the decoder fix later committed as 3bf88fb. In both
  browsers they measured libzstd without ruzstd and checked correctness again. In Chromium they
  traced where libzstd's lead on session50 comes from. They are at the end of this document.
- **A Chromium rerun** at e4ffc99, started 2026-11-09 at 11:03 UTC.

By the rerun, tickwave-wasm inflated with libzstd (3d17dd1). Its modules had also shrunk. Since
e4ffc99 the viewer parses the sidecar's TOML in JavaScript and hands `resolveAnnotations` JSON, so
the TOML parser is out of the default build. Both builds lost a JSON writer, the code serde used to
buffer channel headers, and the parts of zstd-sys's libc shim that decoding never calls (b32e9a6,
4ce1120, 61b8eca). The benchmark's reader fetched the first 32 KiB of a recording rather than
64 KiB, the change committed as 396aeb5. The recordings had been converted again for the rename,
which made every head 2 bytes longer and left every block the same size.

The rerun's tables replace the protocol run's Chromium tables for first inset, block decode and
correctness. Those older tables are in this file's history, at 0f94dc4. The rerun did not repeat
the checksum or whole-file measures, so those tables are still the protocol run's.

Firefox was not rerun. Other work kept the machine too busy for the runner's quiet gate, and the
Firefox run was stopped. Every Firefox table here is the protocol run's, at 48d7f58, where
tickwave-wasm inflates with ruzstd. In those tables, the libzstd rows use the zstd library that
tickwave-wasm uses now.

What the rerun changed in Chromium:

- The default build's transfer fell from 530,231 bytes to 376,414 (209,697 to 150,972 gzipped).
- On Slow 4G, the default build's first inset arrives 500-512 ms sooner on five of the six
  recordings. Two changes produced that. Its download is 58,725 bytes smaller gzipped, and the first
  read is 32 KiB shorter. The worker downloads both at once, over the same throttled link. fzstd's
  worker barely changed, and its first inset arrives 174-179 ms sooner on the same five. That is
  what the shorter read saves on its own.
- gcc-swd-matrix-capture100000 is the exception. Its head is 53,724 bytes, so a 32 KiB first read
  leaves the rest of the head to a second fetch. On Slow 4G that fetch costs a round trip. Its first
  inset is 189 ms later than in the protocol run with the default build, 499 ms later with the
  decoder-only build, and 517 ms later with fzstd.
- Block-decode rates from the two runs are not comparable, because the machine's load differed
  between them. Compare decoders within one run.

## How it was measured

This is the method fixed for the protocol run. The follow-ups and the Chromium rerun used the same
harness; "The runs" and the follow-ups' own section say where they differ.

Machine: AMD Ryzen AI Max+ 395 (16 cores, 32 threads), Linux 7.3.1. Browsers through Playwright
1.65.0, headless: Chromium 158.0.8127.18 (Playwright's headless shell, build 1254) and Firefox
159.0 (build 1572). Node 26.13.0.

**Recordings.** Six recordings, converted from Saleae Logic 2 exports with `tickwave convert saleae`
and its defaults: zstd level 19 and 1 MiB blocks. They are not in this repository. session50 is the
analog recording. It stores its samples as 12-bit codes in two byte planes: every code's low byte,
then every code's high byte.

| Recording | Blocks | Overview blocks | Edges | Samples | File |
| --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | 7 | 4 | 6,791,956 | | 0.39 MB |
| stock-reads | 22 | 17 | 14,324,188 | | 1.40 MB |
| swo-24000000 | 48 | 15 | 43,655,997 | | 6.02 MB |
| gcc-oscan1-probe-rs | 303 | 117 | 238,898,043 | | 11.1 MB |
| gcc-swd-matrix-capture100000 | 742 | 452 | 637,277,314 | | 49.4 MB |
| session50 | 382 | 5 | 0 | 200,004,000 | 108 MB |

**Decoders.**

- tickwave-wasm, default build: `crates/wasm` as it builds by default, with the sidecar code, which
  then included a TOML parser. `build.sh` builds it as `crates/wasm/build.sh` does. The module came
  out byte-identical to the one `crates/wasm/build.sh` writes to `crates/wasm/pkg` from the same
  commit.
- libzstd in WASM (`bench/libzstd`): tickwave-wasm's decoder surface, with libzstd inflating blocks
  through `Head::decode_block_with`. zstd-sys compiles libzstd for `wasm32-unknown-unknown` with
  clang, against its own shim headers, which hand `malloc`, `memcpy` and `qsort` to Rust. Nothing
  else was needed. In the protocol run, `Head::parse` inflated the overview with ruzstd, so this
  module carried both zstd decoders. `decode_overview_block` took no inflate, so this build could
  not decode overview blocks. The follow-ups removed both limits.
- fzstd with the TypeScript reader (`src/format.ts`). fzstd skips the content checksum, so XXH64
  from hash-wasm, a small WASM module, checks it as a separate step.
- `DecompressionStream("zstd")` with the same reader. Chromium 158 does not have it. Firefox 159
  has it behind `dom.compression_streams.zstd.enabled`, off by default. The runs turn it on, so
  its rows show what Firefox would do if it shipped. No visitor's browser has it on today.

Each decoder runs in a Web Worker, as the viewer's does. esbuild minifies every bundle. A local
static server sends each gzip -9 and answers byte ranges. The server isolates the page cross-origin
(COOP and COEP), so timers resolve to microseconds rather than Firefox's 1 ms.

**First inset.** Every run launches a fresh browser. In Firefox, a new context in a running browser
is not cold for a WASM decoder. With `scripts/cold-check.ts`, 8 runs each, tickwave-wasm is ready
in a median 58.7-60.2 ms in a fresh browser, against 9.0-12.9 ms in a new context. Chromium takes
4-5 ms either way. Most of that cost falls on the first module a Firefox process compiles, not on
each module.

The worker starts fetching and compiling its decoder, and at the same time fetches the recording's
first 64 KiB (32 KiB in the rerun). Once the decoder is ready, it fetches the rest of the head if
the first read did not hold all of it. Then it parses the head, fetches the middle block by range
and decodes it. The page draws the block as a polyline, one vertical stroke per pixel column, and
reads back one pixel to force the raster. The time runs from navigation start to that point.

Throttled runs use DevTools' "Slow 4G" through the Chrome DevTools Protocol: 562.5 ms latency,
1.6 Mbit/s × 0.9 down, 750 kbit/s × 0.9 up (`front_end/core/sdk/NetworkManager.ts`,
`Slow4GConditions`, devtools-frontend `f439efd2`). It reaches the worker's fetches: a 70,825-byte
block took 963 ms, against the 956 ms the profile predicts. Firefox has no equivalent through
Playwright.

**Block decode.** Each decoder has its own worker, all alive at once, each holding the whole file.
One pass decodes every block once. After one discarded pass each, 20 passes alternate between the
decoders, with the order rotated each round. The worker times its own decode, and the page times
the round trip from its request to the arrays arriving back.

**Whole file.** The worker decodes every block of gcc-swd-matrix-capture100000 in turn, keeping
nothing, 20 times per decoder in rotation, after one discarded run.

**Statistics.** Percentiles are nearest-rank over every sample pooled. For a rate, "p95" means the
slow side: the rate that 95 % of samples reach. "What is compared" defines the A-A floor.

**Other load.** Other work shared the machine. Each timed unit (one cold inset, one pass, one
whole-file run) waited for three seconds in a row with the CPUs less than 10 % busy, but for 5 s at
most: after that it started anyway (see the break tests), so some units ran on a busy machine. The tables
give the median and maximum busy fraction while the units ran. That includes the run's own work,
such as launching a browser. A unit under a second counts the second it started in. The native
timing ran behind the same gate, with a load average of 0.94 before and 1.31 after.

## Results

The tables are `scripts/report.ts`'s output, from two runs. Chromium's come from the rerun at
e4ffc99, except the checksum and whole-file tables, which it did not repeat. Firefox's tables, and
those two, come from the protocol run at 48d7f58. report.ts then labelled both tickwave-wasm builds
"(ruzstd)", from before the switch; in the rerun's tables both inflate with libzstd. report.ts's
"vs ruzstd" column compares with the decoder-only build.

The protocol run:

Machine: AMD RYZEN AI MAX+ 395 w/ Radeon 8060S, 32 threads; Node v26.13.0; run started 2026-11-09T00:06:27.011Z.
Decoders built from tickwave 48d7f58 with no uncommitted changes to crates/format or crates/wasm. Modules served, SHA-256: libzstd/tickwave_wasm_libzstd_bg.wasm ba096641f6cc3fd6..., ruzstd/tickwave_wasm_bg.wasm 3a0060f992d1e5f3..., ruzstd-full/tickwave_wasm_bg.wasm df0ee3bf3cf6d328....

The Chromium rerun:

Machine: AMD RYZEN AI MAX+ 395 w/ Radeon 8060S, 32 threads; Node v26.13.0; run started 2026-11-09T11:03:57.213Z.
Decoders built from tickwave e4ffc99 with no uncommitted changes to crates/format or crates/wasm. The page and the benchmark's reader read a first 32 KiB, a change uncommitted then and committed after the run as 396aeb5. Modules served, SHA-256: libzstd/tickwave_wasm_libzstd_bg.wasm 3014e9c9b00099da..., probe-libzstd/tickwave_wasm_libzstd_bg.wasm f4f7067edd5ce841..., probe-ruzstd/tickwave_wasm_libzstd_bg.wasm 6e591916cd28f5c8..., ruzstd/tickwave_wasm_bg.wasm 30a8e9d1944dbe65..., ruzstd-full/tickwave_wasm_bg.wasm cf93e667f3b38b33....

### Decoder transfer size

At e4ffc99, for the Chromium tables:

| decoder | files | bytes | gzip -9 |
| --- | --- | --- | --- |
| tickwave-wasm, default build (ruzstd) | tickwave_wasm_bg.wasm, worker.js | 376414 | 150972 |
| tickwave-wasm, decoder only (ruzstd) | tickwave_wasm_bg.wasm, worker.js | 261301 | 108267 |
| libzstd in WASM | tickwave_wasm_libzstd_bg.wasm, worker.js | 249833 | 101666 |
| fzstd + TypeScript | worker.js | 31138 | 12887 |
| DecompressionStream("zstd") + TypeScript | worker.js | 14728 | 5552 |

Every decoder also loads the page (inset.html, inset.js: 1220 bytes gzipped).

At 48d7f58, for the Firefox tables:

| decoder | files | bytes | gzip -9 |
| --- | --- | --- | --- |
| tickwave-wasm, default build (ruzstd) | tickwave_wasm_bg.wasm, worker.js | 530231 | 209697 |
| tickwave-wasm, decoder only (ruzstd) | tickwave_wasm_bg.wasm, worker.js | 271369 | 111913 |
| libzstd in WASM | tickwave_wasm_libzstd_bg.wasm, worker.js | 357954 | 136489 |
| fzstd + TypeScript | worker.js | 30130 | 12559 |
| DecompressionStream("zstd") + TypeScript | worker.js | 13720 | 5219 |

Every decoder also loads the page (inset.html, inset.js: 1219 bytes gzipped).

### Against the draft bar

The first-inset and block-decode tables below hold every figure the draft bar compares, and "What
the numbers say" gives its verdicts.

### First inset, chromium 158.0.8127.18, unthrottled, at e4ffc99

Milliseconds from navigation start to the drawn inset, median (p95) of 20 cold runs. A-A floor 2.2 %; "vs ruzstd" compares medians with the decoder-only build.

| recording | decoder | drawn | worker up | decoder ready | block fetched | decode | vs ruzstd |
| --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, default build (ruzstd) | 70.7 (75.3) | 32.9 | 43.5 | 50.3 | 12.4 | same |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 72.0 (78.8) | 33.0 | 43.3 | 50.3 | 12.8 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 72.8 (86.2) | 32.8 | 43.2 | 50.4 | 13.3 |  |
| gcc-swd-reads | libzstd in WASM | 73.5 (80.9) | 32.6 | 43.4 | 51.0 | 13.4 | same |
| gcc-swd-reads | fzstd + TypeScript | 76.6 (81.8) | 33.6 | 40.9 | 45.5 | 23.8 | +6 % (worse) |
| stock-reads | tickwave-wasm, default build (ruzstd) | 66.1 (69.9) | 32.2 | 42.8 | 49.7 | 9.7 | same |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 65.7 (67.7) | 32.0 | 42.3 | 49.5 | 9.6 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 65.6 (70.9) | 32.5 | 42.7 | 49.5 | 9.4 |  |
| stock-reads | libzstd in WASM | 65.2 (69.5) | 31.6 | 41.6 | 49.0 | 10.2 | same |
| stock-reads | fzstd + TypeScript | 64.5 (67.6) | 33.3 | 40.3 | 47.6 | 11.1 | same |
| swo-24000000 | tickwave-wasm, default build (ruzstd) | 70.4 (74.6) | 32.4 | 43.2 | 50.1 | 13.4 | same |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 69.8 (71.8) | 31.8 | 42.1 | 49.1 | 13.9 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 69.6 (71.7) | 32.1 | 42.1 | 49.2 | 13.4 |  |
| swo-24000000 | libzstd in WASM | 70.7 (74.2) | 31.8 | 41.7 | 49.4 | 13.8 | same |
| swo-24000000 | fzstd + TypeScript | 73.6 (75.7) | 32.6 | 39.6 | 45.1 | 22.4 | +5 % (worse) |
| gcc-oscan1-probe-rs | tickwave-wasm, default build (ruzstd) | 67.6 (70.8) | 32.2 | 42.6 | 49.7 | 11.1 | same |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 66.9 (70.1) | 31.8 | 42.0 | 48.9 | 10.6 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 65.9 (68.9) | 31.5 | 41.4 | 47.9 | 10.9 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 67.2 (70.2) | 32.1 | 41.9 | 48.9 | 11.4 | same |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 76.7 (79.1) | 32.7 | 40.1 | 45.9 | 24.4 | +15 % (worse) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, default build (ruzstd) | 70.5 (74.5) | 32.1 | 42.2 | 50.0 | 12.7 | same |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 71.2 (73.0) | 31.9 | 42.5 | 50.5 | 13.0 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 69.6 (73.5) | 32.3 | 41.8 | 50.3 | 12.3 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 70.7 (74.0) | 31.9 | 41.9 | 50.5 | 12.9 | same |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 78.1 (82.5) | 33.1 | 40.6 | 49.9 | 21.1 | +10 % (worse) |
| session50 | tickwave-wasm, default build (ruzstd) | 66.6 (69.3) | 32.8 | 43.6 | 52.0 | 4.5 | same |
| session50 | tickwave-wasm, decoder only (ruzstd) | 66.1 (68.9) | 32.7 | 42.9 | 51.3 | 4.7 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 66.1 (68.5) | 32.2 | 43.0 | 51.4 | 4.8 |  |
| session50 | libzstd in WASM | 66.1 (68.9) | 32.1 | 42.0 | 51.3 | 4.6 | same |
| session50 | fzstd + TypeScript | 77.9 (82.2) | 32.9 | 40.1 | 56.1 | 12.7 | +18 % (worse) |

Block: the middle one. cross-origin isolated: true.

Bytes fetched a recording, head then the middle block: gcc-swd-reads 1876 + 70825; stock-reads 14960 + 33308; swo-24000000 9674 + 126783; gcc-oscan1-probe-rs 19422 + 36536; gcc-swd-matrix-capture100000 53724 + 79047; session50 22087 + 265181. Machine busy during the runs (this run included): median 8 %, max 14 %.

### First inset, firefox 159.0, unthrottled, at 48d7f58

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

### First inset, chromium 158.0.8127.18, DevTools "Slow 4G", at e4ffc99

Milliseconds from navigation start to the drawn inset, median (p95) of 20 cold runs. A-A floor 0.0 %; "vs ruzstd" compares medians with the decoder-only build.

| recording | decoder | drawn | worker up | decoder ready | block fetched | decode | vs ruzstd |
| --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, default build (ruzstd) | 4324.1 (4339.5) | 1761.1 | 3335.9 | 4301.3 | 13.4 | +6 % (worse) |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 4090.8 (4106.5) | 1760.7 | 3102.0 | 4067.9 | 13.2 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 4090.6 (4100.7) | 1760.4 | 3102.1 | 4068.1 | 13.3 |  |
| gcc-swd-reads | libzstd in WASM | 4058.0 (4065.6) | 1761.1 | 3068.8 | 4034.5 | 14.0 | -1 % (better) |
| gcc-swd-reads | fzstd + TypeScript | 3556.7 (3566.1) | 1803.3 | 1806.7 | 3526.4 | 22.3 | -13 % (better) |
| stock-reads | tickwave-wasm, default build (ruzstd) | 4108.6 (4117.4) | 1760.7 | 3335.3 | 4092.6 | 9.2 | +6 % (worse) |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 3875.8 (3892.1) | 1760.6 | 3102.3 | 3859.3 | 9.5 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 3875.6 (3892.2) | 1760.7 | 3102.2 | 3859.2 | 9.1 |  |
| stock-reads | libzstd in WASM | 3843.6 (3851.7) | 1760.6 | 3068.7 | 3826.0 | 10.2 | -1 % (better) |
| stock-reads | fzstd + TypeScript | 3336.2 (3345.2) | 1803.6 | 1807.3 | 3317.4 | 12.9 | -14 % (better) |
| swo-24000000 | tickwave-wasm, default build (ruzstd) | 4631.2 (4639.5) | 1760.6 | 3335.5 | 4609.4 | 13.8 | +5 % (worse) |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 4397.8 (4406.1) | 1760.6 | 3102.1 | 4376.2 | 13.9 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 4397.8 (4414.5) | 1760.6 | 3102.1 | 4375.9 | 14.3 |  |
| swo-24000000 | libzstd in WASM | 4364.8 (4373.6) | 1760.6 | 3068.7 | 4342.7 | 14.6 | -1 % (better) |
| swo-24000000 | fzstd + TypeScript | 3865.2 (3872.2) | 1803.5 | 1806.9 | 3834.4 | 23.9 | -12 % (better) |
| gcc-oscan1-probe-rs | tickwave-wasm, default build (ruzstd) | 4128.7 (4143.0) | 1760.5 | 3335.6 | 4109.6 | 11.5 | +6 % (worse) |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 3894.8 (3903.7) | 1760.4 | 3102.4 | 3875.8 | 11.2 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 3895.2 (3903.6) | 1760.7 | 3102.3 | 3876.1 | 11.5 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 3862.7 (3872.3) | 1761.0 | 3069.0 | 3843.2 | 11.8 | -1 % (better) |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 3367.5 (3374.9) | 1803.6 | 1807.1 | 3334.2 | 25.8 | -14 % (better) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, default build (ruzstd) | 5055.0 (5063.8) | 1760.6 | 3335.3 | 5034.2 | 12.5 | +5 % (worse) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 4822.4 (4837.3) | 1760.7 | 3102.2 | 4801.7 | 12.4 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 4822.8 (4839.7) | 1760.7 | 3102.2 | 4801.5 | 12.9 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 4789.2 (4804.4) | 1760.2 | 3068.7 | 4767.6 | 13.0 | -1 % (better) |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 4288.8 (4300.1) | 1803.7 | 1806.9 | 4259.3 | 22.2 | -11 % (better) |
| session50 | tickwave-wasm, default build (ruzstd) | 5400.7 (5409.8) | 1760.3 | 3335.6 | 5384.9 | 5.0 | +5 % (worse) |
| session50 | tickwave-wasm, decoder only (ruzstd) | 5165.9 (5173.5) | 1760.0 | 3101.6 | 5151.0 | 4.8 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 5166.7 (5174.5) | 1760.6 | 3102.0 | 5151.2 | 4.9 |  |
| session50 | libzstd in WASM | 5133.7 (5149.8) | 1760.5 | 3068.2 | 5117.7 | 5.0 | -1 % (better) |
| session50 | fzstd + TypeScript | 4641.8 (4652.6) | 1803.1 | 1806.8 | 4618.0 | 14.1 | -10 % (better) |

Block: the middle one. cross-origin isolated: true.

Bytes fetched a recording, head then the middle block: gcc-swd-reads 1876 + 70825; stock-reads 14960 + 33308; swo-24000000 9674 + 126783; gcc-oscan1-probe-rs 19422 + 36536; gcc-swd-matrix-capture100000 53724 + 79047; session50 22087 + 265181. Machine busy during the runs (this run included): median 1 %, max 14 %.

### Block decode, chromium 158.0.8127.18, at e4ffc99

Millions of edges a second (session50: of samples), median (p95, slow side) over 20 runs of every block. "In worker": decode inside the worker, to typed arrays. "Round trip": the page's request to the arrays arriving back, the worker hop included. A-A floor 1.8 % in worker, 1.4 % round trip.

| recording | decoder | in worker | round trip | hop ms | in worker vs ruzstd |
| --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd) | 178.6 (157.2) | 172.7 (150.6) | 0.15 |  |
| gcc-swd-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 180.3 (156.6) | 173.9 (151.0) | 0.13 |  |
| gcc-swd-reads | libzstd in WASM | 171.9 (155.2) | 167.2 (147.0) | 0.14 | -4 % (worse) |
| gcc-swd-reads | fzstd + TypeScript | 101.2 (91.9) | 99.8 (90.1) | 0.12 | -43 % (worse) |
| stock-reads | tickwave-wasm, decoder only (ruzstd) | 136.9 (114.6) | 134.7 (101.7) | 0.10 |  |
| stock-reads | tickwave-wasm, decoder only (ruzstd), A-A copy | 139.3 (115.4) | 136.7 (105.9) | 0.10 |  |
| stock-reads | libzstd in WASM | 137.7 (113.5) | 135.1 (104.2) | 0.10 | same |
| stock-reads | fzstd + TypeScript | 86.4 (59.4) | 84.6 (55.2) | 0.13 | -37 % (worse) |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd) | 151.1 (136.2) | 148.2 (132.1) | 0.12 |  |
| swo-24000000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 151.2 (136.2) | 148.1 (131.9) | 0.11 |  |
| swo-24000000 | libzstd in WASM | 145.1 (130.1) | 142.3 (125.7) | 0.12 | -4 % (worse) |
| swo-24000000 | fzstd + TypeScript | 97.5 (90.4) | 96.4 (88.7) | 0.10 | -36 % (worse) |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd) | 172.6 (129.1) | 168.0 (122.3) | 0.11 |  |
| gcc-oscan1-probe-rs | tickwave-wasm, decoder only (ruzstd), A-A copy | 172.1 (129.1) | 167.2 (123.5) | 0.11 |  |
| gcc-oscan1-probe-rs | libzstd in WASM | 166.4 (129.9) | 161.7 (124.1) | 0.11 | -4 % (worse) |
| gcc-oscan1-probe-rs | fzstd + TypeScript | 115.9 (81.5) | 112.2 (77.1) | 0.09 | -33 % (worse) |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd) | 170.4 (124.1) | 166.9 (119.7) | 0.10 |  |
| gcc-swd-matrix-capture100000 | tickwave-wasm, decoder only (ruzstd), A-A copy | 168.8 (124.1) | 165.3 (119.9) | 0.10 |  |
| gcc-swd-matrix-capture100000 | libzstd in WASM | 164.2 (125.3) | 160.7 (120.9) | 0.11 | -4 % (worse) |
| gcc-swd-matrix-capture100000 | fzstd + TypeScript | 107.2 (66.5) | 104.2 (60.2) | 0.14 | -37 % (worse) |
| session50 | tickwave-wasm, decoder only (ruzstd) | 283.4 (170.2) | 273.1 (163.3) | 0.06 |  |
| session50 | tickwave-wasm, decoder only (ruzstd), A-A copy | 283.4 (169.9) | 273.8 (163.6) | 0.06 |  |
| session50 | libzstd in WASM | 282.6 (170.8) | 273.1 (163.8) | 0.06 | same |
| session50 | fzstd + TypeScript | 70.6 (61.2) | 69.7 (60.0) | 0.09 | -75 % (worse) |

Machine busy during the runs (this run included): median 5 %, max 16 %.

Median ms a block by step, every recording pooled:

| decoder | steps |
| --- | --- |
| tickwave-wasm, decoder only (ruzstd) | decode 3.41, copyOut 1.36 |
| libzstd in WASM | decode 3.51, copyOut 1.36 |
| fzstd + TypeScript | inflate 2.81, checksum 0.09, parse 4.73 |

### Block decode, firefox 159.0, at 48d7f58

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

### Checksum (XXH64 over the decoded block), chromium, at 48d7f58

stock-reads, 20 runs of each of its 1 MiB blocks (the last is shorter).

| decoder | checksum ms median | p95 | whole block ms median |
| --- | --- | --- | --- |
| fzstd + TypeScript | 0.09 | 0.11 | 7.21 |
| fzstd + TypeScript, pure-JS XXH64 | 19.95 | 25.22 | 27.58 |

### Checksum (XXH64 over the decoded block), firefox, at 48d7f58

stock-reads, 20 runs of each of its 1 MiB blocks (the last is shorter).

| decoder | checksum ms median | p95 | whole block ms median |
| --- | --- | --- | --- |
| fzstd + TypeScript | 0.10 | 0.12 | 5.48 |
| fzstd + TypeScript, pure-JS XXH64 | 21.88 | 22.42 | 27.24 |

### The largest recording decoded whole (gcc-swd-matrix-capture100000, 742 blocks, 637,277,314 edges), at 48d7f58

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

What happens to a block's bytes, as the code does it (read in the wasm-bindgen glue, tickwave-wasm
and the TypeScript):

| Decoder | Into WASM memory | Inside | Out of WASM memory | Hop to the page |
| --- | --- | --- | --- | --- |
| tickwave-wasm, libzstd | the compressed frame (`&[u8]`: malloc and copy) | inflate to a 1 MiB payload; parse to `Vec<i64>` edges; `edges()` maps them to a new `Vec<f64>` | each channel's `Vec<f64>` or `Vec<f32>` copied to a new JS array (`Float64Array::from`) | the arrays transferred, no copy |
| fzstd + TypeScript | the 1 MiB payload, copied into hash-wasm's memory for XXH64 | n/a | n/a | the same |
| DecompressionStream + TypeScript | none | n/a | n/a | the same |

The JavaScript readers write edges straight into the `Float64Array` they return. The stream's
output chunks are copied once into one payload buffer. The request to the worker is a few bytes,
and in the block measure the worker already holds the file.

The step tables give what this costs. Getting a block's arrays out of WASM memory (the i64-to-f64
pass, the copy out, and freeing the block) takes a median 1.36 ms in Chromium at e4ffc99 and
1.14 ms in Firefox at 48d7f58. That is 29 % and 16 % of tickwave-wasm's time a block. The hop to
the page and back takes a median 0.06-0.15 ms in Chromium and 0.10-0.22 ms in Firefox.

## Correctness

Every block went through every decoder in both browsers: the six recordings' blocks and the one
block of identify-swd, a small recording. Each block's output was hashed (SHA-256 over the
start tick, each channel's level, edges as i64 and values' f32 bits) and compared with
`Head::decode_block` natively. Every overview block went the same way, against
`Head::decode_overview_block`, in every decoder that can decode one. The TypeScript reader shares
no code with the Rust, so where the two agree, each checks the other.

The corrupted blocks all come from the middle block of gcc-swd-reads (block 3) and of session50
(block 191), made by `tickwave-bench-native corrupt`:

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

The frame kinds are decoded against the file's own head. The others are whole files, with the
index and preamble moved to fit. Each decoder first decodes an untouched block of the same file, so
a refusal cannot come from a broken setup.

### Correctness, chromium 158.0.8127.18, at e4ffc99

The rerun also has the nine kinds the follow-ups added, dictionary to code-too-wide, described
under "3. Correctness at the new base". It records where each refusal happened: opening the head,
or decoding the block.

DecompressionStream("zstd"): no by default. Cross-origin isolated: true.

| decoder | blocks hashed | differ from native | overview blocks hashed | differ from native |
| --- | --- | --- | --- | --- |
| tickwave-wasm, default build (ruzstd) | 1505 | 0 | 610 | 0 |
| tickwave-wasm, decoder only (ruzstd) | 1505 | 0 | 610 | 0 |
| libzstd in WASM | 1505 | 0 | 610 | 0 |
| fzstd + TypeScript | 1505 | 0 | 610 | 0 |
| fzstd + TypeScript, pure-JS XXH64 | 1505 | 0 | 610 | 0 |

| recording | corruption | refuse at | native refusal | tickwave-wasm, default build (ruzstd) | tickwave-wasm, decoder only (ruzstd) | libzstd in WASM | fzstd + TypeScript | fzstd + TypeScript, pure-JS XXH64 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| gcc-swd-reads | flip-data | block | zstd: checksum mismatch | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd content checksum a841ba4d does not match c8efab53 | refused at block: Error: zstd content checksum a841ba4d does not match c8efab53 |
| gcc-swd-reads | flip-checksum | block | zstd: checksum mismatch | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd content checksum c8efab53 does not match c9efab53 | refused at block: Error: zstd content checksum c8efab53 does not match c9efab53 |
| gcc-swd-reads | no-checksum | block | a frame carries no checksum | refused at block: Error: a frame carries no checksum | refused at block: Error: a frame carries no checksum | refused at block: Error: a frame carries no checksum | refused at block: Error: zstd frame has no content checksum | refused at block: Error: zstd frame has no content checksum |
| gcc-swd-reads | zero-interval | block | block 3, channel SWCLK: two edges share a tick | refused at block: Error: block 3, channel SWCLK: two edges share a tick | refused at block: Error: block 3, channel SWCLK: two edges share a tick | refused at block: Error: block 3, channel SWCLK: two edges share a tick | refused at block: Error: block 3 channel 0: a zero interval after the first | refused at block: Error: block 3 channel 0: a zero interval after the first |
| gcc-swd-reads | trailing-frame | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 8 bytes follow the zstd frame | refused at block: Error: zstd frame is 70825 bytes, followed by 8 more | refused at block: Error: zstd frame is 70825 bytes, followed by 8 more |
| gcc-swd-reads | trailing-empty-frame | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 13 bytes follow the zstd frame | refused at block: Error: zstd frame is 70825 bytes, followed by 13 more | refused at block: Error: zstd frame is 70825 bytes, followed by 13 more |
| gcc-swd-reads | flip-data-then-empty-frame | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 13 bytes follow the zstd frame | refused at block: Error: zstd frame is 70825 bytes, followed by 13 more | refused at block: Error: zstd frame is 70825 bytes, followed by 13 more |
| gcc-swd-reads | trailing-frame-checksum | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 12 bytes follow the zstd frame | refused at block: Error: zstd frame is 70825 bytes, followed by 12 more | refused at block: Error: zstd frame is 70825 bytes, followed by 12 more |
| gcc-swd-reads | decoded-longer | block | a frame decoded to 1048584 bytes, not 1048585 | refused at block: Error: a frame decoded to 1048584 bytes, not 1048585 | refused at block: Error: a frame decoded to 1048584 bytes, not 1048585 | refused at block: Error: a frame decoded to 1048584 bytes, not 1048585 | refused at block: Error: zstd frame decoded to 1048584 bytes, not 1048585 | refused at block: Error: zstd frame decoded to 1048584 bytes, not 1048585 |
| gcc-swd-reads | decoded-shorter | block | zstd: Target must have at least as many bytes as the contentsize of the frame reports | refused at block: Error: zstd: Destination buffer is too small | refused at block: Error: zstd: Destination buffer is too small | refused at block: Error: zstd: Destination buffer is too small | refused at block: Error: zstd frame decoded to 1048584 bytes, not 1048583 | refused at block: Error: zstd frame decoded to 1048584 bytes, not 1048583 |
| gcc-swd-reads | dictionary | block | zstd: Frame header specified dictionary id 0x2A that wasnt provided by add_dict() or reset_with_dict() | refused at block: Error: a frame names a dictionary | refused at block: Error: a frame names a dictionary | refused at block: Error: a frame names a dictionary | refused at block: Error: zstd frame names a dictionary | refused at block: Error: zstd frame names a dictionary |
| gcc-swd-reads | no-overview | head | head: the header lists no overview level | refused at head: Error: the header lists no overview level | refused at head: Error: the header lists no overview level | refused at head: Error: the header lists no overview level | refused at head: Error: overview: the head holds at least one level | refused at head: Error: overview: the head holds at least one level |
| gcc-swd-reads | level-bit | head | head: channel SWCLK: block 3 starts at the wrong level | refused at head: Error: channel SWCLK: block 3 starts at the wrong level | refused at head: Error: channel SWCLK: block 3 starts at the wrong level | refused at head: Error: channel SWCLK: block 3 starts at the wrong level | refused at head: Error: channel SWCLK: block 3's level bit does not follow from initial and the edge counts | refused at head: Error: channel SWCLK: block 3's level bit does not follow from initial and the edge counts |
| gcc-swd-reads | length-before-end | head | head: channel SWCLK ends outside the capture | refused at head: Error: channel SWCLK ends outside the capture | refused at head: Error: channel SWCLK ends outside the capture | refused at head: Error: channel SWCLK ends outside the capture | refused at head: Error: channel SWCLK: end 927702610 is outside 0..=127888190 | refused at head: Error: channel SWCLK: end 927702610 is outside 0..=127888190 |
| gcc-swd-reads | edge-after-end | block | block 6, channel SWCLK: an edge lies outside the block | refused at block: Error: block 6, channel SWCLK: an edge lies outside the block | refused at block: Error: block 6, channel SWCLK: an edge lies outside the block | refused at block: Error: block 6, channel SWCLK: an edge lies outside the block | refused at block: Error: block 6 channel 0: edge at 806314527 is past the block | refused at block: Error: block 6 channel 0: edge at 806314527 is past the block |
| gcc-swd-reads | edge-at-tick-0 | block | block 0, channel SWCLK: an edge lies outside the block | refused at block: Error: block 0, channel SWCLK: an edge lies outside the block | refused at block: Error: block 0, channel SWCLK: an edge lies outside the block | refused at block: Error: block 0, channel SWCLK: an edge lies outside the block | refused at block: Error: block 0 channel 0: an edge at tick 0 | refused at block: Error: block 0 channel 0: an edge at tick 0 |
| session50 | flip-data | block | zstd: checksum mismatch | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd content checksum ffd8baa0 does not match 89b5a223 | refused at block: Error: zstd content checksum ffd8baa0 does not match 89b5a223 |
| session50 | flip-checksum | block | zstd: checksum mismatch | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd: Restored data doesn't match checksum | refused at block: Error: zstd content checksum 89b5a223 does not match 88b5a223 | refused at block: Error: zstd content checksum 89b5a223 does not match 88b5a223 |
| session50 | no-checksum | block | a frame carries no checksum | refused at block: Error: a frame carries no checksum | refused at block: Error: a frame carries no checksum | refused at block: Error: a frame carries no checksum | refused at block: Error: zstd frame has no content checksum | refused at block: Error: zstd frame has no content checksum |
| session50 | section-shift | block | block 191, channel SWCLK: the section does not hold its samples | refused at block: Error: block 191, channel SWCLK: the section does not hold its samples | refused at block: Error: block 191, channel SWCLK: the section does not hold its samples | refused at block: Error: block 191, channel SWCLK: the section does not hold its samples | refused at block: Error: block 191 channel 0: 524284 bytes for 262144 codes | refused at block: Error: block 191 channel 0: 524284 bytes for 262144 codes |
| session50 | trailing-frame | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 8 bytes follow the zstd frame | refused at block: Error: zstd frame is 265181 bytes, followed by 8 more | refused at block: Error: zstd frame is 265181 bytes, followed by 8 more |
| session50 | trailing-empty-frame | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 13 bytes follow the zstd frame | refused at block: Error: zstd frame is 265181 bytes, followed by 13 more | refused at block: Error: zstd frame is 265181 bytes, followed by 13 more |
| session50 | flip-data-then-empty-frame | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 13 bytes follow the zstd frame | refused at block: Error: zstd frame is 265181 bytes, followed by 13 more | refused at block: Error: zstd frame is 265181 bytes, followed by 13 more |
| session50 | trailing-frame-checksum | block | the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: the bytes hold more than one frame | refused at block: Error: 12 bytes follow the zstd frame | refused at block: Error: zstd frame is 265181 bytes, followed by 12 more | refused at block: Error: zstd frame is 265181 bytes, followed by 12 more |
| session50 | decoded-longer | block | a frame decoded to 1048588 bytes, not 1048589 | refused at block: Error: a frame decoded to 1048588 bytes, not 1048589 | refused at block: Error: a frame decoded to 1048588 bytes, not 1048589 | refused at block: Error: a frame decoded to 1048588 bytes, not 1048589 | refused at block: Error: zstd frame decoded to 1048588 bytes, not 1048589 | refused at block: Error: zstd frame decoded to 1048588 bytes, not 1048589 |
| session50 | decoded-shorter | block | zstd: Target must have at least as many bytes as the contentsize of the frame reports | refused at block: Error: zstd: Destination buffer is too small | refused at block: Error: zstd: Destination buffer is too small | refused at block: Error: zstd: Destination buffer is too small | refused at block: Error: zstd frame decoded to 1048588 bytes, not 1048587 | refused at block: Error: zstd frame decoded to 1048588 bytes, not 1048587 |
| session50 | dictionary | block | zstd: Frame header specified dictionary id 0x2A that wasnt provided by add_dict() or reset_with_dict() | refused at block: Error: a frame names a dictionary | refused at block: Error: a frame names a dictionary | refused at block: Error: a frame names a dictionary | refused at block: Error: zstd frame names a dictionary | refused at block: Error: zstd frame names a dictionary |
| session50 | no-overview | head | head: the header lists no overview level | refused at head: Error: the header lists no overview level | refused at head: Error: the header lists no overview level | refused at head: Error: the header lists no overview level | refused at head: Error: overview: the head holds at least one level | refused at head: Error: overview: the head holds at least one level |
| session50 | bits-17 | head | head: channel SWCLK's codes are 17 bits | refused at head: Error: channel SWCLK's codes are 17 bits | refused at head: Error: channel SWCLK's codes are 17 bits | refused at head: Error: channel SWCLK's codes are 17 bits | refused at head: Error: channel SWCLK: bits 17 is outside 1..=16 | refused at head: Error: channel SWCLK: bits 17 is outside 1..=16 |
| session50 | period-zero | head | head: channel SWCLK's period has a zero term | refused at head: Error: channel SWCLK's period has a zero term | refused at head: Error: channel SWCLK's period has a zero term | refused at head: Error: channel SWCLK's period has a zero term | refused at head: Error: channel SWCLK: period [0,1] has a term that is not above zero | refused at head: Error: channel SWCLK: period [0,1] has a term that is not above zero |
| session50 | code-too-wide | block | block 191, channel SWCLK: code 65368 is wider than 12 bits | refused at block: Error: block 191, channel SWCLK: code 65368 is wider than 12 bits | refused at block: Error: block 191, channel SWCLK: code 65368 is wider than 12 bits | refused at block: Error: block 191, channel SWCLK: code 65368 is wider than 12 bits | refused at block: Error: code 65368 is wider than 12 bits | refused at block: Error: code 65368 is wider than 12 bits |

### Correctness, firefox 159.0, at 48d7f58

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

### A defect in the native decoder: one frame a block was not enforced

3bf88fb ("format: refuse anything but one checksummed frame a block") fixed this after the protocol
run. `inflate` now refuses unless the last frame decoded spans every byte it was given. The native
decoder from before the fix accepts 7 of the protocol run's 14 corrupted frames (the frame kinds,
in both files). It accepts every trailing kind in both files, except gcc-swd-reads'
flip-data-then-empty-frame, which the block parser refuses. With the fix it refuses all 14.

The Firefox table above is from before the fix, and the Chromium table from after it. The protocol
run's Chromium table gave the same verdicts as the Firefox one for every decoder the two share.

What the defect let through in the protocol run, natively and in both tickwave-wasm builds in
both browsers:

- A block's frame followed by a skippable frame, empty or not, or by an empty zstd frame: accepted.
- session50's block 191 with one bit flipped in its frame, followed by an empty zstd frame. The
  flipped frame alone fails with "checksum mismatch". With the empty frame after it, it was
  accepted and decoded to wrong values. Its hash differed from the reference's, and no error was
  raised.

The libzstd build and the JavaScript readers refuse all of these.

## Break tests

Each check was broken once on purpose and seen to fail, then restored from a saved copy. Unless the
table says otherwise, each ran in Chromium, with the hash checks on identify-swd and gcc-swd-reads
and every corruption of gcc-swd-reads and session50. All but the overview-block row ran before the
recordings were converted again with overview blocks, on the files and decoder of that time. The
checks they break did not change with the conversion.

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
- A real network or a CDN. The server is local, HTTP/1.1, gzip -9. GitHub Pages' own gzip level is
  unknown.
- Throttling in Firefox, which Playwright cannot do, and any profile but Slow 4G.
- A slower CPU. DevTools' CPU slowdown was not used. A mid-range phone's CPU would stretch decode
  and compile times, and that could move the inset comparison either way.
- WebKit, any mobile browser, and Chromium's full build in its new headless mode. The runs use
  Playwright's headless shell.
- Memory: the WASM heap's growth, and the copies' peak.
- `DecompressionStream("zstd")` as visitors would get it. No browser ships it on by default.
- Other builds: libzstd's `thin` feature (smaller, slower), wasm-opt `-Oz`, and WASM SIMD.
- A pure-JavaScript XXH64 written for speed. The two published ones I found work in BigInt
  (`@intrnl/xxhash64`) or in `cuint` objects (`xxhashjs`). The checksum table has the BigInt one:
  about 20 ms a 1 MiB block, against 0.1 ms for hash-wasm's WASM one.
- A recording shorter than the first read, through the worker. The worker expects a full first
  read, 64 KiB in the protocol run and 32 KiB in the rerun, and would refuse a shorter file. Every
  recording here is longer.
- The renderer. The stand-in draws one polyline, and the viewer's drawing costs will differ.

## Where FORMAT.md fell short for an independent reader

The TypeScript reader was written from FORMAT.md before reading the Rust. Where the two then
differed, FORMAT.md was silent or loose. 3bf88fb settled all nine points below in FORMAT.md, and
the Rust now refuses each case: an edge outside `1..=end`, a tick past `length`, block starts that
do not strictly increase, a code at or above `2^bits`, anything but one frame, a frame naming a
dictionary, and a head with no overview level. FORMAT.md now states that an f32 is little-endian.
The Rust also checks every block's level bit against `initial` and the edge counts before it.

1. An edge at tick 0. "Only the first [interval] may be zero" allowed a zero first interval in
   block 0, which puts an edge at tick 0. The Rust refuses any edge before tick 1 (`tick < 1`).
   FORMAT.md did not say which was right.
2. Edges after a channel's `end`. The Rust refuses an edge past the channel's `end`, and accepts
   one at `end`. FORMAT.md called `end` "the tick up to which the level is known" and said neither.
   The reader written from the spec bounded edges by `length`.
3. How large a tick can be. The header promises integers below 2^53, but nothing bounded a tick in
   a block except, by implication, `length`. A JavaScript reader needs every tick below 2^53.
   "Every tick is at most `length`" would have settled it, with item 2.
4. Index order. FORMAT.md said "in time order", and the Rust refuses two blocks with the same
   start. "Strictly increasing" would have said so.
5. f32 byte order. FORMAT.md said "All integers are little-endian", and f32 values are not
   integers. Analog f32 samples and the overview's minimums and maximums are little-endian in the
   Rust.
6. Codes wider than `bits`. FORMAT.md did not say what a code at or above 2^`bits` means. The Rust
   accepted one; the TypeScript reader refused it.
7. One frame a block. "One zstd frame a block" did not say that nothing may follow the frame, or
   whether skippable frames may. The native decoder accepted bytes after the frame (see the defect
   above).
8. An empty overview list. The Rust refuses a header with no overview level. The level rule
   implied at least one, and FORMAT.md did not say so.
9. Frames that name a dictionary. No dictionary exists, so such a frame cannot be read. FORMAT.md
   did not forbid a dictionary id.

Two observations rather than gaps. The reader checks that block 0's level bit equals the header's
`initial`, which the Rust did not then check. And the fixed 64 KiB first fetch costs about 930 ms
on Slow 4G (562.5 ms latency plus 364 ms of transfer), while the heads here are 1.8-46 KB.
396aeb5 has since halved the first fetch to 32 KiB.

## Follow-ups after the protocol run (2026-11-09)

The protocol run left three questions. How does the libzstd build compare once ruzstd is out of
it? Is libzstd's lead on session50 in inflating or in the block parser? And does every decoder
still pass the correctness checks on the fixed decoder, with both tickwave-wasm builds now refusing
all four trailing kinds? tickwave-wasm here still inflates with ruzstd. These
results come between the protocol run and the Chromium rerun. They do not repeat the protocol
run's tables.

**Base.** `main` at 6b82dda, plus the decoder fix then staged in `crates/format` and since
committed as 3bf88fb. It adds `Head::parse_with` and `Head::decode_overview_block_with`, so the
head's overview and overview blocks can be inflated by another zstd decoder, as blocks already
could. The recordings are the ones converted again on 2026-11-08 at 23:50, checked unchanged at
the start and end of every run.

**Load.** Other work was converting recordings on every core. No timed measure started until the
1-minute load average had stayed below 2 for five minutes. Every timed unit still waited for three
quiet seconds, now with no time limit.

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
On Slow 4G that makes its first inset 28-39 ms later, against 131-144 ms before. Its decoder is
ready 33.6 ms after tickwave-wasm's (3,344 against 3,310 ms), close to the 32 ms that 5,842 bytes
take at 180,000 bytes a second. Unthrottled, the two are within the floor on 7 of 12 rows. libzstd
is 2-10 % sooner on four of the other five (10 % on session50 in Chromium), and 3 % later on
gcc-oscan1-probe-rs in Chromium.

In block decode, libzstd is now faster than tickwave-wasm on every recording in both browsers:
7-31 % on the digital ones in Chromium and 12-41 % in Firefox. Linked with ruzstd, at 48d7f58, it
had been 12 % slower to 5 % faster in Chromium. Two things changed between those builds: ruzstd
left, and the staged decoder checks arrived. Nothing here separates the two.

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

### 2. Where libzstd's lead on session50 comes from

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

session50's gap is in inflating. ruzstd takes 4.97 ms to inflate a block, and libzstd 0.64 ms,
7.8 times as fast. The parse takes 0.78 ms in both. On session50 the ruzstd probe totals 6.24 ms
against tickwave-wasm's 6.21, so there the probe stands in for tickwave-wasm.

gcc-swd-reads, a digital recording, is here for contrast. libzstd inflates it faster too (0.73
against 1.81 ms), and the parse again takes the same time in both probes. But the ruzstd probe
totals 7.90 ms against tickwave-wasm's 6.38. Splitting the call costs about 1.5 ms a block on
digital blocks, and nothing measurable on session50's. The probes compare fairly with each other,
but their digital parse time is not tickwave-wasm's. Each probe call also copies the frame into
WASM memory, one copy more than `decodeBlock` makes.

### 3. Correctness at the new base

Every block and overview block of every recording went through every decoder, in both browsers,
against `Head::decode_block` and `Head::decode_overview_block` built from the staged decoder:

| Browser | Decoders | Blocks each | Differ | Overview blocks each | Differ |
| --- | --- | --- | --- | --- | --- |
| Chromium 158.0.8127.18 | 7: both tickwave-wasm builds, libzstd, both probes, fzstd, fzstd with pure-JS XXH64 | 1,505 | 0 | 610 | 0 |
| Firefox 159.0 | 8: the same and DecompressionStream | 1,505 | 0 | 610 | 0 |

The corrupted blocks are the ten kinds above plus one for each rule FORMAT.md settled: 31 in all,
16 on gcc-swd-reads and 15 on session50. Every decoder refused every one, 217 refusals in Chromium
and 248 in Firefox, each at the stage the corruption targets. Both tickwave-wasm builds now refuse
all four trailing kinds ("the bytes hold more than one frame"). Each new kind breaks one rule and
keeps every length:

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

Each refusal is recorded with where it happened: opening the head, decoding a control block the
corruption does not touch, or decoding the target block. A refusal anywhere but the target counts
against the decoder.

### Break tests for the reader's new checks

The rules FORMAT.md settled gave the TypeScript reader new checks. Each was removed in turn, the
reader run through the new kinds (Chromium, fzstd), and the check restored from a saved copy:

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

The last three checks are backed by others, which refuse the same input for another reason. The
stage check failed once by accident. The first edge-after-end lowered `end` so far that the control
block was refused too, and the run reported "refused at control, expected block".

### Correction: what libzstd costs inside tickwave-wasm (2026-11-09)

The size comparison in §1 sets the libzstd crate in `bench/libzstd` against tickwave-wasm. The two
crates do not carry the same code: tickwave-wasm also carries about 16 KB of float formatting from
Rust's `core` library, which the libzstd crate does not. Measured within one crate instead: at
6b82dda plus the staged fix, `crates/wasm` built with `--no-default-features` and the same
`wasm-opt` flags is 261,676 bytes (108,914 gzipped) with ruzstd, and 313,570 bytes (123,777
gzipped) with libzstd. The switch costs 14.9 KB gzipped, about 83 ms on Slow 4G at 180,000 B/s,
not 5.8 KB and 32 ms. The decode speeds above are unaffected. Where the float formatting comes from
was not traced here.
