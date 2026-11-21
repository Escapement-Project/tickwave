# tickwave

tickwave is a web viewer for mixed-signal captures. It draws digital channels from their edges and
analog channels from their samples, on one time axis. A page cites a stretch of a capture the way it
cites a figure: a phrase in the text opens an inset of that stretch, and the whole capture is a
click away.

It is built to sit inside a page. Its custom elements take the page's colours and fonts, size with
its layout, and fetch only the part of a recording a view shows. Nothing runs on a server: any
static host serves a page and its recordings, and the viewer also opens files from the reader's
computer.

## What is here

| Path | What |
| --- | --- |
| `crates/format` | the `.tickwave` recording format: encoding, decoding, the block index, and the sidecar annotations' resolver |
| `crates/import` | readers for Saleae Logic 2's binary export, sigrok session files and VCD, and the conversion the CLI and the browser share |
| `crates/cli` | `tickwave`, the command-line converter: convert, describe, verify, check and bundle |
| `crates/wasm` | the same decoder compiled to WASM, for the browser |
| `crates/convert-wasm` | the converter compiled to WASM, for the site's Open page |
| `crates/view` | the viewer's logic apart from the drawing: a recording opened finished or read while it is written, a view's columns, samples, statistics and markers, and the web's readouts, times, value scale, time axis and point probes |
| `crates/egui` | the native renderer: a recording drawn with egui through `crates/view`, as a graph of a trace and digital lanes with a strip of the whole recording under it, for verge's desktop app; with its `decode` feature, protocol decoders' items as tracks under the graph, and a side panel for the one selected |
| `crates/decode` | the contract a protocol decoder is written against, and the host that runs protocol decoders over a recording, checks what they emit and stacks one on another's output |
| `crates/decoders` | the reference protocol decoders, written against `crates/decode`'s contract alone: SWD, JTAG and cJTAG, with JTAG-DP, ADIv5 and MEM-AP stacked on them |
| `web` | the custom elements, the chart, the converter's worker, and the build of the site |
| `site` | the site's pages: an overview, a viewer that also converts, and the docs |
| `bench` | the decode benchmark: the WASM decoder against JavaScript ones, on real recordings |
| `fuzz` | a fuzz target for each reader, for the writer, for each stack of reference protocol decoders and for their host, in a workspace of its own |

## Documents

- `FORMAT.md`: the recording format, version 1: how a writer streams it, and how a reader reads it
  from its end, in order, or repairs one cut off.
- `ANNOTATIONS.md`: the sidecar annotations that sit beside a recording.
- `CHARTS.md`: `<tickwave-chart>` and its data.
- `DECODING.md`: protocol decoding: how a decoder runs in the host, what the host checks, and how
  a view gets a window's items.
- `web/README.md`: the elements, how they read recordings, and how to run the web build and tests.
- `testdata/view/README.md`: the cases the web's view modules and `crates/view` share.
- `site/docs/converter.md`: building and using the converter.
- `bench/README.md` and `bench/RESULTS.md`: how the benchmark runs, and what it found.

## Using a release

### The elements

Each release puts the elements' files on tickwave's site, in a directory named after its version:
`https://escapement-project.github.io/tickwave/v0.1.2/`. A published directory never changes, and
its `SHA256SUMS` lists each file's SHA-256 digest. A page on any site loads the elements from
there:

```html
<link rel="stylesheet" href="https://escapement-project.github.io/tickwave/v0.1.2/tickwave.css">
<script type="module" src="https://escapement-project.github.io/tickwave/v0.1.2/tickwave.js"></script>
```

To serve them from the page's own host instead, copy the whole directory. `web/README.md` covers
the elements themselves, and what a page with a Content-Security-Policy allows.

### The Rust crates

The crates aren't on crates.io. Depend on them by git, at a release's tag:

```toml
[dependencies]
escapement-tickwave-format = { git = "https://github.com/Escapement-Project/tickwave", tag = "v0.1.2" }
```

`escapement-tickwave-view` and `escapement-tickwave-egui` come the same way.

## Building and testing

The Rust crates need a current Rust toolchain and a C compiler, for libzstd. The wasm32 build takes
clang with the wasm32 target:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p escapement-tickwave-view --target wasm32-unknown-unknown --no-default-features
cargo build -p escapement-tickwave-decode --target wasm32-unknown-unknown --no-default-features
cargo build --release -p escapement-tickwave-cli     # the converter, at target/release/tickwave
```

The native renderer's demo opens a recording, or records a synthetic one live:

```sh
cargo run --release -p escapement-tickwave-egui --features decode --example demo -- web/fixtures/energy.tickwave
cargo run --release -p escapement-tickwave-egui --features decode --example demo -- --live 100000
```

The browser's decoder and converter also need `wasm-bindgen` and `wasm-opt`:

```sh
crates/wasm/build.sh                      # the decoder, into crates/wasm/pkg
crates/convert-wasm/build.sh              # the converter, into crates/convert-wasm/pkg
```

Then the web build and its tests, from `web/`:

```sh
npm install
npx playwright install chromium firefox   # once, for the browser tests
npm run check                             # typecheck, unit tests, build and browser tests
```

The fuzz targets build with cargo-fuzz on nightly, from `fuzz/`. `seeds` writes each target's seed
corpus from the fixtures and testdata. A reader target takes a structured input that it builds
into a file with its checks right, so mutations get past the checksums:

```sh
cargo install cargo-fuzz
cd fuzz
cargo run --no-default-features --bin seeds -- .. seeds
mkdir -p corpus/reader                                   # libFuzzer adds what it finds here
cargo +nightly fuzz run reader corpus/reader seeds/reader -- \
    -rss_limit_mb=2048 -malloc_limit_mb=1024 -timeout=10
cargo run --no-default-features --bin show -- <input>   # the file an input builds, as read
```

The protocol decoder targets take a structured input too, and have no seeds. `decode_swd`,
`decode_jtag` and `decode_cjtag` build the lines a stack of reference decoders reads, and `host`
builds the decoders it runs:

```sh
mkdir -p corpus/decode_swd
cargo +nightly fuzz run decode_swd corpus/decode_swd -- \
    -rss_limit_mb=2048 -malloc_limit_mb=1024 -timeout=10
```

The converter carries the viewer for `tickwave open` when it's built after the web build, with the
`viewer` feature:

```sh
cargo build --release -p escapement-tickwave-cli --features viewer
```

## Licence

MIT or Apache-2.0, at your choice: `LICENSE-MIT` and `LICENSE-APACHE`.

tickwave's icon and the project's dial in `site/` (`tickwave.svg`, `tickwave-mark.json`, `dial.svg`
and `favicon-tickwave.ico`) are the Escapement Project's marks, under CC BY 4.0 in
`site/CC-BY-4.0.txt`. Credit the Escapement Project. Use the marks to refer to the project or its
tools. Don't use them, changed or not, as another project's mark, or to suggest that the project
made or endorses something it didn't. The `tickwave` binary built with the `viewer` feature
carries them, so its crate's licence is `(MIT OR Apache-2.0) AND CC-BY-4.0`. The fonts in
`site/fonts/` are under the SIL Open Font License, each beside its text.
