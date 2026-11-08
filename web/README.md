# chrono's web elements

The canvas renderer, its interaction, and the three custom elements: `<chrono-view>`,
`<chrono-inset>` and `<chrono-ref>`. They draw from a small data-source interface,
`src/source.ts`, not from the file format. The WASM decoder will sit behind that interface. Until
it does, the dev page reads fixtures cut from real captures.

## Running it

From `web/`:

```sh
npm install
npx playwright install chromium firefox   # once, for the browser tests
node scripts/vendor.ts                    # tokens.css and fonts for the dev page, into dev/vendor/
node scripts/fixtures.ts                  # the fixtures, from the project's exports
npm run dev                               # http://127.0.0.1:8123/dev/
```

| Command | Does |
|---|---|
| `npm run build` | `dist/chrono.js`, the elements as one ES module, and `dist/dev-loader.js` |
| `npm run typecheck` | `tsc` over the sources, tests and scripts |
| `npm test` | the unit tests, with Node's test runner |
| `npm run e2e` | Playwright in Chromium and Firefox; screenshots go to `screenshots/` |
| `npm run check` | all four, in that order |

The committed fixture, `fixtures/identify-swd.chronofx`, is enough for the unit tests and most of
the browser tests. The stress cases and the screenshots need the ignored `fixtures-local/`, which
`scripts/fixtures.ts` writes from the exports. Tests that need it skip without it.

## The data source

`src/source.ts` is the renderer's only view of a recording. Every method is async, and the renderer
never waits on one: it draws the data it has and redraws when more arrives. Channels are indices
into `info().channels`, and ticks are integers below 2^53 counted from the capture's start.

- `info()`: the timebase in ticks a second, the length, the title, the download size, and each
  channel's name, kind and end tick. An analog channel adds its unit, first tick, sample period,
  sample count, code resolution and value range.
- `edges(channel, start, end)`: the level at `start` and the edges in `[start, end)`.
- `overview(channel, start, end, maxBucket)`: the coarsest level no coarser than `maxBucket`, or
  null when there is none. The renderer then reads edges or samples.
- `samples(channel, start, end)`: an analog channel's values.
- `prefetch()`: the head, ahead of a view.

A page or the decoder registers a loader for the URLs it reads (`registerLoader` in `src/index.ts`).
Every element on the page shares one source per URL.

## Fixture files

`scripts/extract-fixtures.ts` writes `.chronofx` files from Saleae binary exports and from verge's
energy capture CSV. A fixture is gzip over:

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

Ticks follow the converter's rule, `round((t − begin) × rate)`. The extractor refuses an edge more
than 0.01 of a tick off the grid, and an export whose channels disagree on `begin` or rate.

`scripts/swd-annotations.ts` decodes SWD in a fixture and writes a generator's annotation file
(JSON Lines, PLAN.md §3): line resets, the JTAG-to-SWD select, and each transfer with its request,
acknowledge and data. The hand-written `fixtures/identify-swd.annotations.json` names some of
them for the dev page's prose.

## The elements

Import `chrono.js` and the three elements are defined.

`<chrono-view>` takes `src`, `annotations`, `channels`, `window`, `open` and `series`. Collapsed,
it shows one line and an Open button and fetches nothing. The line names the file until the
recording's head is here (a reference on the page fetches it). Open, the view takes these controls:

- wheel and trackpad pinch zoom about the pointer
- drag and the arrow keys pan; Shift moves further
- `+` and `−` zoom, Home fits the capture, Escape clears the cursors and the selection
- two fingers pinch
- a click places cursor A, then B; it snaps to an edge within 6 px on the clicked row
- a drag on the time axis selects a span
- clicking a note shows its span

Methods are `show(id)`, `setWindow(start, end)` (ticks, or times such as `"317.8 ms"`), `settled()`
and `tickAt(clientX)`. Events are `chrono-open`, `chrono-window`, `chrono-select`, `chrono-ready`
and `chrono-error`.

`<chrono-inset>` takes `src`, `annotations`, `annotation`, `channels`, `pad` (`"6%"` of the span by
default, or a time), `scale` (`"50 ns/px"`), `sweep` (`off`) and `series`. It draws the annotations
inside its span as notes. It sweeps in once per appearance, and an inset shown again within
800 ms of hiding does not sweep. `measure` gives `rate`, `period` (the median), `edges` or
`duration`. It fires `chrono-ready` once it has drawn.

`<chrono-ref>` takes `annotation`, and `for` or its own `src` and `annotations`. It borrows `src`,
`annotations`, `channels` and `series` from its view.

Colours come from the page's custom properties, with system colours where a page has none:

- `--ink`, `--ink-2`, `--hair`, `--rule`, `--page`, `--surface`, `--raised` and `--wash`
- `--series-<name>` for `series`
- `--chrono-<role>` for a `paint` role. `probe` falls back to the series colour and every other
  role to ink.

Fonts come from `--font-text` and `--font-figures`. Parts are `summary`, `title`, `facts`,
`viewer`, `toolbar`, `plot`, `readout`, `steps`, `button` and `open-button` on the view; `inset`,
`label`, `frame`, `canvas`, `foot`, `measure` and `key` on the inset; `popover` on the reference.
