# chrono's web elements

The canvas renderer, its interaction, and the three custom elements: `<chrono-view>`,
`<chrono-inset>` and `<chrono-ref>`. They draw from a small data-source interface,
`src/source.ts`. Behind it, `.chrono` recordings are read by the Rust core compiled to WASM, in a
worker; the dev page's in-memory fixtures are read by a second loader.

## Running it

The web build copies the WASM decoder from `crates/wasm/pkg`, so build that first, from the
repository root: `crates/wasm/build.sh`. Then, from `web/`:

```sh
npm install
npx playwright install chromium firefox   # once, for the browser tests
node scripts/vendor.ts                    # tokens.css and fonts for the dev page, into dev/vendor/
node scripts/fixtures.ts                  # the local fixtures, from the project's exports
npm run dev                               # http://127.0.0.1:8123/dev/
```

| Command | Does |
|---|---|
| `npm run build` | `dist/`: `chrono.js`, `chrono-worker.js`, `chrono.wasm` and the dev page's `dev-loader.js` |
| `npm run generate` | the files the tests generate rather than commit (below) |
| `npm run typecheck` | `tsc` over the sources, tests and scripts |
| `npm test` | the unit tests, with Node's test runner |
| `npm run e2e` | generates, then runs Playwright in Chromium and Firefox; screenshots go to `screenshots/` |
| `npm run check` | typecheck, unit tests, build and e2e, in that order |

The browser tests run against `e2e/server.ts`, which serves `web/` twice: on port 8124 it honours
HTTP Range, on 8125 it ignores it. It logs every request, a worker's included, so a test can check
what was fetched.

The stress cases and the screenshots need the ignored `fixtures-local/`, which
`scripts/fixtures.ts` writes from the exports. Tests that need it skip without it.

## Shipping

A page imports `chrono.js`. It loads `chrono-worker.js` and `chrono.wasm` from beside itself, the
first time something needs a recording. The three files go together.

## The data source

`src/source.ts` is the renderer's only view of a recording. Every method is async, and the renderer
never waits on one: it draws the data it has and redraws when more arrives. Channels are indices
into `info().channels`, and ticks are integers below 2^53 counted from the capture's start.

- `info()`: the timebase in ticks a second, the length, the title, the download size, and each
  channel's name, kind and end tick. An analog channel adds its unit, first tick, sample period,
  sample count, the value of one code step (null for a source that stored floats) and its range.
- `edges(channel, start, end)`: the level at `start` and the edges in `[start, end)`.
- `overview(channel, start, end, maxBucket)`: the coarsest level no coarser than `maxBucket`, or
  null when there is none. The renderer then reads edges or samples.
- `samples(channel, start, end)`: an analog channel's values. Sample i sits at
  `first + floor(i × period)`.
- `prefetch()`: the head, ahead of a view.

A loader reads the recordings at the URLs it accepts (`registerLoader`). Every element on the page
shares one source per URL.

## Reading `.chrono` files

`chrono.js` registers a loader for `.chrono` URLs, and for local files made into URLs with
`registerFile(file)`. Each recording is opened once, in one worker that holds the WASM module.

- The head comes by HTTP Range: the first 64 KiB, then the rest if the head is longer. Data blocks
  and overview blocks come by Range as a view needs them. Adjacent missing blocks share one
  request, and a block inside the first read is not fetched again.
- A server that answers 200 instead of 206 has sent the whole file. It is kept, and every later
  read is served from it.
- A 206 with a `Content-Encoding` is refused: its ranges count bytes of the compressed stream.
- A local file reads through `File.slice`.
- Decoded blocks sit in an LRU of 192 MiB, shared by every recording on the page.
- An aborted request stops at once, and so does a read no other request is waiting on.
- `configure({ firstRead, cacheBytes })` changes the first read and the cache's size.

An element's `annotations` attribute names a sidecar's TOML file. The worker fetches it and the
files it includes, relative to its URL, and resolves them with the Rust core. Without the
attribute, a recording with a bundled sidecar uses that. A sidecar's problems go to the console and
to a short notice in the element; the recording still draws.

Resolved annotations keep sidecar order: the hand-written file's, then each included file's. The
order says which kind a view steps through first. The lane and the stepping sort by time
themselves. An alias carries `ref`, the generated annotation it names, which the lane then draws
once, as the alias.

## Fixtures

Committed, in `fixtures/`:

| File | What it is |
|---|---|
| `identify-swd.chrono` | `chrono convert saleae testdata/saleae/identify-swd --channel digital_3_0=SWCLK --channel digital_3_1=SWDIO` |
| `identify-swd.4k.chrono` | the same with `--block-kib 4`: nine blocks, for edges across block boundaries |
| `identify-jtag.chrono` | the `identify-jtag-cjtag` export's TCK, TDI and TMS (`digital_4_0`, `_4_2`, `_4_3`): long enough that its 2^12 level sits in overview blocks |
| `swd50-cut.chrono` | `chrono convert saleae testdata/saleae/swd50-cut --channel analog_0=SWCLK --channel analog_1=SWDIO --channel digital_4_0=NRST`: 50 µs of the MSO's analog inputs as 12-bit codes, in an 11 ms capture |
| `identify-swd.annotations.toml` | the hand-written sidecar for `identify-swd.chrono` |
| `identify-swd.chronofx` | the in-memory route's fixture, cut from the same export by `scripts/extract-fixtures.ts` |

Generated by `scripts/generate.ts`, not committed: `identify-swd.swd.jsonl`, the SWD decoder's
annotations of `identify-swd.chrono` with its `recording_id`, which the TOML file includes; and
`identify-swd.bundle.chrono`, the recording with its sidecar bundled, where the converter is built.

`scripts/swd-annotations.ts` decodes SWD in a `.chrono` (through the same WASM code, in Node) or a
`.chronofx`: line resets, the JTAG-to-SWD select, and each transfer with its request, acknowledge
and data.

### `.chronofx` files

The dev page's in-memory route, `src/memory-source.ts`, serves `.chronofx` fixtures with an
artificial delay on request (`?latency=MS`), and computes the overview at the levels a real file
has. A fixture is gzip over:

| Bytes | Holds |
|---|---|
| 8 | `chronofx` |
| 4 | the header's length, u32 little-endian |
| header | JSON: `format` (`chrono-fixture/1`), `title`, `timebase` `[num, den]`, `length`, `channels`, `source` |
| payloads | each channel's, in header order, each `bytes` long |

A digital channel's header entry has `name`, `end`, `initial` (the level at tick 0), `count` and
`bytes`. Its payload is unsigned LEB128: the first edge's tick, then each interval to the next
edge. An analog channel's entry has `name`, `end`, `unit`, `scale`, `offset`, `first`, `period`
`[num, den]`, `count` and `bytes`. Its payload is zigzag LEB128: the first integer code, then each
difference. A value is `code × scale + offset`.

The dev page's stress sections read these by default. `dev/?stress=chrono` switches them to the
chrono session's converted recordings, linked into `fixtures-local/` by `scripts/fixtures.ts`.

## The elements

Import `chrono.js` and the three elements are defined.

`<chrono-view>` takes `src`, `annotations`, `channels`, `window`, `open` and `series`. Collapsed,
it shows one line and an Open button. When it scrolls into view it fetches the recording's head,
and only the head, to say what the recording is. Open, the view takes these controls:

- Ctrl or Cmd with the wheel, and a trackpad pinch, zoom about the pointer; the plain wheel
  scrolls the page and shows how to zoom
- Shift with the wheel, a horizontal swipe, a drag and the arrow keys pan
- `+` and `−` zoom, Home fits the capture, Escape clears the cursors and the selection
- two fingers pinch
- a click places cursor A, then B; it snaps to an edge within 6 px on the clicked row
- a drag on the time axis selects a span
- clicking a note shows its span
- the kind menu and its arrows step through one kind of annotation, "n of N"; the kind is the one
  the view was opened at, else the first annotation's

Cursors read out their times, the interval between them, and each analog channel's value at
each, the last sample at or before it, to the digit one code step carries. A selection shows as a
bar along the time axis, above its labels.

A selection reads out its length, each channel's edges, and for one channel the periods' count,
minimum, median and maximum, and the running clock. The running clock is the mean of the periods
within one tick of the median: a quantised clock's median sits on one sample step, and
first-to-last counts the gaps between bursts.

Methods are `show(id)`, `setWindow(start, end)` (ticks, or times such as `"317.8 ms"`), `settled()`
and `tickAt(clientX)`. Events are `chrono-open`, `chrono-window`, `chrono-select`, `chrono-ready`
and `chrono-error`.

`<chrono-inset>` takes `src`, `annotations`, `annotation`, `channels`, `pad` (`"6%"` of the span by
default, or a time), `scale` (`"50 ns/px"`), `sweep` (`off`) and `series`. It shows the
annotation's label and text, and draws the annotations inside its span as notes. It sweeps in once
per appearance, and an inset shown again within 800 ms of hiding does not sweep. `measure` gives
the running clock's `rate` or `period`. It fires `chrono-ready` once it has drawn.

`<chrono-ref>` takes `annotation`, and `for` or its own `src` and `annotations`. It borrows `src`,
`annotations`, `channels` and `series` from its view.

Colours come from the page's custom properties, with system colours where a page has none:

- `--ink`, `--ink-2`, `--hair`, `--rule`, `--page`, `--surface`, `--raised`, `--wash` and
  `--attention-ink` (for a notice)
- `--series-<name>` for `series`
- `--chrono-<role>` for a `paint` role. `probe` falls back to the series colour and every other
  role to ink. The key reads "probe drives" and "target drives", and any other role by its name.

Fonts come from `--font-text` and `--font-figures`. Parts are `summary`, `title`, `facts`,
`viewer`, `toolbar`, `plot`, `hint`, `readout`, `steps`, `notice`, `button` and `open-button` on the
view; `inset`, `label`, `text`, `frame`, `canvas`, `foot`, `measure`, `key` and `notice` on the
inset; `popover` on the reference.
