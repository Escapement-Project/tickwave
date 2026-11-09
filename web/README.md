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
| `npm run site` | chrono's site into `dist/site/`, from `site/` at the repository's root (below) |
| `npm run e2e` | generates, then runs Playwright in Chromium and Firefox; screenshots go to `screenshots/` |
| `npm run check` | typecheck, unit tests, build and e2e, in that order |

The browser tests run against `e2e/server.ts`, which serves `web/` twice: on port 8124 it honours
HTTP Range, on 8125 it ignores it. It logs every request, a worker's included, so a test can check
what was fetched.

The stress cases and the screenshots need the ignored `fixtures-local/`, which
`scripts/fixtures.ts` writes from the exports. Tests that need it skip without it.

## The site

chrono's site lives in `site/` at the repository's root, and `scripts/site.ts` builds it into
`dist/site/`. `npm run build` builds it too. The site is static, and every URL in it is relative,
so it works under any path, such as `/chrono/` on GitHub Pages. Its pages:

- `index.html`, the viewer: a recording by its address (`?src=`, `?annotations=`) or from files
- `demo.html`, a short page citing the committed fixtures as a project page would
- `docs/elements.html`, rendered from this file's "The elements" section
- `docs/format.html` and `docs/annotations.html`, rendered from `FORMAT.md` and `ANNOTATIONS.md`
- `docs/converter.html`, rendered from `site/docs/converter.md`

`site/tokens.css`, `site/fonts/` and `site/favicon.ico` are copies of the design system's files.
Check them from the site repository with
`uv run design/build.py --check ../chrono/site/tokens.css ../chrono/site/fonts/* ../chrono/site/favicon.ico`.

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

chrono puts a capture into a web page with three custom elements. `<chrono-view>` shows a whole
recording. `<chrono-inset>` shows one annotated stretch of it, magnified. `<chrono-ref>` turns a
phrase in the text into a reference: pointing at it opens an inset beside the text.

### Loading

Include one module, and the three elements are defined:

```html
<script type="module" src="chrono.js"></script>
```

`chrono.js` reads recordings through a decoder in a worker. The first time a page needs a
recording, it loads `chrono-worker.js` and `chrono.wasm` from beside itself, so keep the three
files together. Recordings are fetched by byte range: the head first, then only the blocks a view
shows. A server that ignores ranges still works, as long as it sends the whole file.

### `<chrono-view>`

A whole recording. Closed, it shows one line: the recording's title, its length to three
significant figures, its channels and its size, and an Open button. It fetches the recording's
head, and nothing else, once it scrolls into view. Open, it shows the time axis, one row a
channel, the annotations above them, and a readout under them.

| Attribute | Means |
|---|---|
| `src` | the recording's URL |
| `annotations` | its sidecar's TOML file. Without it, a recording with a bundled sidecar shows that |
| `channels` | the channels to show, by name, in order. All of them by default |
| `window` | what an open view shows first: two times (`"317.8 ms 318.0 ms"`), or an annotation's id |
| `open` | open the view |
| `series` | a series name, such as `verge`: the traces take `--series-verge`. Ink by default |

Times in attributes are a decimal with a unit (`s`, `ms`, `us`, `ns`, `ps`), or a whole number of
ticks.

The reader zooms with Ctrl or Cmd and the wheel, a trackpad pinch, or two fingers. The plain wheel
scrolls the page, and the view says how to zoom. Shift with the wheel, a sideways swipe, a drag or
the arrow keys move along the capture. With the view focused, plus and minus zoom, Home shows the
whole capture, and Escape clears the cursors and the selection.

A click places cursor A, and the next click cursor B. A cursor snaps to an edge within 6 pixels on
the row clicked. The readout gives both times, the interval between them, and each analog
channel's value at each. That value is the last sample at or before the cursor, to the digit one
code step carries.

A drag along the time axis selects a span. The readout gives its length and each channel's edges
in it. For one channel it gives the periods' count, minimum, median and maximum, and the running
clock. The running clock is the mean of the periods inside Tukey's fences, as the sidecar's
measures define it: it keeps a clock's jitter and quantisation, and drops pauses.

A menu beside the zoom buttons steps through one kind of annotation, "3 of 16". It starts with the
kind of the annotation the view opened at, else the kind of the sidecar's first annotation.
Clicking an annotation's label shows its span.

| Method or property | Does |
|---|---|
| `show(id)` | opens the view at an annotation. Resolves once it has drawn |
| `setWindow(start, end)` | shows a span: ticks, or times such as `"317.8 ms"` |
| `settled()` | resolves once the view has drawn everything it shows |
| `tickAt(clientX)` | the tick under a point in the window, or null before the view has drawn |
| `open` | whether the view is open; setting it opens or closes it |
| `window` | the span shown, `{start, end}` in ticks |

| Event | When, and its `detail` |
|---|---|
| `chrono-open` | the view opens: `{src}` |
| `chrono-window` | the span shown changes: `{start, end}` |
| `chrono-select` | a span is selected: its `start`, `end`, `duration`, `edges` by channel, and `periods` for the measured channel |
| `chrono-ready` | the view has first drawn: `{height}` |
| `chrono-error` | something failed: `{message, error}`. The view also shows it, and logs it to the console |

### `<chrono-inset>`

One annotation's span, magnified, with the annotation's label and text above it. The annotations
inside the span show as notes, and an annotation's `paint` colours a line where a role drives it.
An inset sweeps in from the left when it appears. Shown again within 800 ms of hiding, it doesn't
sweep again. A reader who prefers reduced motion sees it drawn at once.

| Attribute | Means |
|---|---|
| `src`, `annotations`, `channels`, `series` | as for `<chrono-view>` |
| `annotation` | the annotation to show, by id. Set it to switch |
| `pad` | room either side of the span: a share of it (`"10%"`, `6%` by default) or a time |
| `scale` | a fixed time scale, such as `"50 ns/px"`, so insets side by side compare |
| `sweep` | `off` to draw at once |

An annotation's `measure` puts a reading under the trace, taken from the channel's own edges:

- `rate` or `period`: the running clock over the span. The note says how many periods it used,
  such as "mean of 31 of 32 periods".
- `duration`: the time from the channel's first edge in the span to its last.

A reading shows only the digits its ticks support. `settled()` and the `chrono-ready` event
(`{annotation, height}`) work as on the view, so a page can lay itself out around an inset's
final height. `sweeping` says whether the sweep is playing.

### `<chrono-ref>`

A phrase that cites an annotation. Pointing at it, or reaching it with Tab, opens an inset in a
popover beside the text, so nothing on the page moves. Moving the pointer or the focus away, or Escape,
closes the popover. A click, or Enter, opens its view at the annotation.

| Attribute | Means |
|---|---|
| `annotation` | the annotation it cites |
| `for` | the id of a `<chrono-view>` to share a recording with. The reference takes its `src`, `annotations`, `channels` and `series` |
| `src`, `annotations`, `series` | the recording, sidecar and series, for a reference without a view |

A reference fetches nothing until it scrolls into view. Then it fetches the recording's head and
the sidecar, and the inset fetches the data when it opens.

### Styling

The elements draw in the page's own colours and fonts. They read the page's CSS custom properties
and fall back to system colours on a page without them:

| Property | Draws |
|---|---|
| `--ink`, `--ink-2` | text, traces, cursors; secondary text |
| `--page`, `--surface`, `--raised` | the backgrounds under the elements and their controls |
| `--hair`, `--rule` | dividers; control outlines |
| `--wash` | what the pointer is on |
| `--attention-ink` | a notice of a problem |
| `--series-<name>` | the traces, with `series="<name>"` |
| `--chrono-<role>` | a line where an annotation's `paint` role drives it. `probe` falls back to the series colour, any other role to ink |
| `--font-text`, `--font-figures` | labels; the readouts' figures |

While a zoom's finer data loads, a row draws the coarser data it has. Where that data shows a
change it cannot yet place, the row draws it in the trace's colour, faded.

The key under an inset names the roles it shows: "probe drives" and "target drives", and any
other role by its name. The elements follow a change of face, a `data-face` on `<html>` or the
system's preference, as it happens.

Each element exposes its pieces to `::part()`. On the view: `summary`, `title`, `facts`,
`open-button`, `viewer`, `toolbar`, `plot`, `hint`, `readout`, `steps`, `notice` and `button`. On
the inset: `inset`, `label`, `text`, `frame`, `canvas`, `foot`, `measure`, `key` and `notice`. On
the reference: `popover`.

### From the module

`chrono.js` also exports three functions:

| Function | Does |
|---|---|
| `registerFile(file, sidecar)` | makes a local `File` a `src`. The sidecar's `.toml` and `.jsonl` files can come with it |
| `recordingInfo(src)` | what a recording holds, from its head: title, length, channels, size, content id |
| `configure({firstRead, cacheBytes})` | the size of the first read of a head, 64 KiB by default; and of the cache of decoded blocks, 192 MiB |
