# tickwave

tickwave shows logic and mixed-signal captures in a web page. It draws digital channels from their
edges and analog channels from their samples, on one time axis. A page cites a stretch of a capture
the way it cites a figure: a phrase in the text opens an inset of that stretch, and the whole
capture is a click away.

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
| `web` | the custom elements, the chart, the converter's worker, and the build of the site |
| `site` | the site's pages: an overview, a viewer that also converts, and the docs |
| `bench` | the decode benchmark: the WASM decoder against JavaScript ones, on real recordings |

## Documents

- `FORMAT.md`: the recording format, version 1.
- `ANNOTATIONS.md`: the sidecar annotations that sit beside a recording.
- `CHARTS.md`: `<tickwave-chart>` and its data.
- `web/README.md`: the elements, how they read recordings, and how to run the web build and tests.
- `site/docs/converter.md`: building and using the converter.
- `bench/README.md` and `bench/RESULTS.md`: how the benchmark runs, and what it found.

## Building and testing

The Rust crates need a current Rust toolchain:

```sh
cargo test --workspace
cargo build --release -p escapement-tickwave-cli     # the converter, at target/release/tickwave
```

The browser's decoder and converter also need clang with the wasm32 target, `wasm-bindgen` and
`wasm-opt`:

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

## Licence

MIT or Apache-2.0, at your choice: `LICENSE-MIT` and `LICENSE-APACHE`.
