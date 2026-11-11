# tickwave's web elements

The canvas renderer, its interaction, and four custom elements: `<tickwave-view>`,
`<tickwave-inset>`, `<tickwave-ref>` and, in a module of its own, `<tickwave-chart>`. The first three
draw from a small data-source interface, `src/source.ts`. Behind it, the Rust core compiled to WASM
reads `.tickwave` recordings in a worker, and a second loader reads the dev page's in-memory
fixtures.

## Running it

The web build copies the WASM decoder from `crates/wasm/pkg` and the converter from
`crates/convert-wasm/pkg`, so build those first, from the repository root: `crates/wasm/build.sh`
and `crates/convert-wasm/build.sh`. Then, from `web/`:

```sh
npm install
npx playwright install chromium firefox   # once, for the browser tests
node scripts/vendor.ts                    # tokens.css and fonts for the dev page, into dev/vendor/
node scripts/fixtures.ts                  # the local fixtures, from the project's exports (optional)
npm run dev                               # http://127.0.0.1:8123/dev/
```

| Command | Does |
|---|---|
| `npm run build` | `dist/`: `tickwave.js`, `tickwave-chart.js`, `tickwave-worker.js`, `tickwave.wasm`, the converter's `tickwave-convert-worker.js` and `tickwave-convert.wasm`, the dev page's `dev-loader.js`, and the site |
| `npm run generate` | the files the tests generate rather than commit (below) |
| `npm run typecheck` | `tsc` over the sources, tests and scripts |
| `npm test` | the unit tests, with Node's test runner. The converter's compare its output with the CLI's, which they run through `cargo` |
| `npm run site` | tickwave's site into `dist/site/`, from `site/` at the repository's root (below) |
| `npm run e2e` | generates, then runs Playwright in Chromium and Firefox; screenshots go to `screenshots/` |
| `npm run check` | typecheck, unit tests, build and e2e, in that order |

The browser tests run against `e2e/server.ts`, which serves `web/` twice: on port 8124 it honours
HTTP Range, on 8125 it ignores it. It logs every request, a worker's included, so a test can check
what was fetched.

The stress cases and the screenshots need the ignored `fixtures-local/`, which
`scripts/fixtures.ts` writes from the Escapement Project's captures, outside this repository. Tests
that need it skip without it.

## The site

tickwave's site lives in `site/` at the repository's root, and `scripts/site.ts` builds it into
`dist/site/`. `npm run build` builds it too. The site is static, and every URL in it is relative,
so it works under any path, such as `/tickwave/` on GitHub Pages. The exception is the project's
shared header, whose places lead to the project's other pages at the published site. tickwave's
own place gives way to its group: tickwave's pages beside its name, with the page shown marked. The
site's pages:

- `index.html`, tickwave's overview, laid out as the project's product pages are: its mark and
  name, a live inset beside them, then a section for each part, each leading to its doc. It cites
  the committed fixtures as a project page would, with references, an inset and two views
- `open.html`, the viewer: a recording by its address (`?src=`, `?annotations=`) or from files, or
  a capture from files, converted in a worker by `tickwave-convert.wasm`, saved and shown
- `docs/index.html`, the docs: each with what it covers, and their search
- `docs/viewer.html`, rendered from `site/docs/viewer.md`: reading a recording in a view
- `docs/elements.html`, rendered from this file's "The elements" section
- `docs/charts.html`, `docs/format.html` and `docs/annotations.html`, rendered from `CHARTS.md`,
  `FORMAT.md` and `ANNOTATIONS.md`
- `docs/converter.html`, rendered from `site/docs/converter.md`

The docs follow the project's docs, in the markup the site's `design/README.md` gives ("A product's
docs"), so they read and work as Verge's do. `scripts/site.ts` writes `docs/search.json`, the
search's index, to that README's definition of a section ("The search index").

`site/tokens.css`, `site/fonts/`, `site/favicon-tickwave.ico`, `site/dial.svg`, `site/tickwave.svg`
(tickwave's mark, on the overview's plate) and `site/tickwave-mark.json` (its motion) are copies of
the design system's files. `site/base.css`, `site/header.html`, `site/footer.html`, `site/face.js`,
`site/header.js`, `site/copy.js`, `site/mark.js`, `site/doc.css`, `site/doc.js` and
`site/search.js` are copies of the project's shared header, footer, base sheet, Copy buttons, the
marks' motion, and the docs' sheet, frame and search. Check them all from the site repository with:

```sh
uv run design/build.py --check ../tickwave/site/tokens.css ../tickwave/site/fonts/* \
    ../tickwave/site/favicon-tickwave.ico ../tickwave/site/dial.svg ../tickwave/site/tickwave.svg \
    ../tickwave/site/tickwave-mark.json ../tickwave/site/base.css ../tickwave/site/header.html \
    ../tickwave/site/footer.html ../tickwave/site/face.js ../tickwave/site/header.js \
    ../tickwave/site/copy.js ../tickwave/site/mark.js ../tickwave/site/doc.css \
    ../tickwave/site/doc.js ../tickwave/site/search.js
```

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

## Reading `.tickwave` files

`tickwave.js` registers a loader for `.tickwave` URLs, in any case, and for local files made into
URLs with `registerFile(file)`. A loader registered later is asked first. While the page loads, a
URL no loader takes waits for one; after that it is refused (`TickwaveNoLoader`). Each recording is
opened once, in one worker that holds the WASM module.

- The head comes by HTTP Range: the first 32 KiB, then the rest if the head is longer. Data blocks
  and overview blocks come by Range as a view needs them. Adjacent missing blocks share one
  request, and a block inside the first read is not fetched again.
- A server that answers 200 instead of 206 has sent the whole file. It is kept, and every later
  read is served from it.
- A 206 with a `Content-Encoding` is refused: its ranges count bytes of the compressed stream.
- A local file reads through `File.slice`.
- Decoded blocks sit in an LRU of 192 MiB, shared by every recording on the page.
- An aborted request stops at once, and so does a read no other request is waiting on.
- `configure({ firstRead, maxDecodedBytes, cacheBytes })` changes the first read, the most the
  decoder takes from one block, and the cache's size.

An element's `annotations` attribute names a sidecar's TOML file. The worker fetches it and the
files it includes, relative to its URL, and resolves them with the Rust core. Without the
attribute, a recording with a bundled sidecar uses that. A sidecar's problems go to the console and
to a short notice in the element, and the recording still draws. A notice goes with its cause: a
new recording or sidecar starts without the last one's.

Resolved annotations keep sidecar order: the hand-written file's, then each included file's. The
order says which kind a view steps through first. The lane and the stepping sort by time
themselves. An alias carries `ref`, the generated annotation it names, which the lane then draws
once, as the alias.

## Fixtures

Committed, in `fixtures/`:

| File | What it is |
|---|---|
| `identify-swd.tickwave` | `tickwave convert saleae testdata/saleae/identify-swd --channel digital_3_0=SWCLK --channel digital_3_1=SWDIO` |
| `identify-swd.4k.tickwave` | the same with `--block-kib 4`: nine blocks, for edges across block boundaries |
| `identify-jtag.tickwave` | the `identify-jtag-cjtag` export's TCK, TDI and TMS (`digital_4_0`, `_4_2`, `_4_3`): long enough that its 2^12 level sits in overview blocks |
| `swd50-cut.tickwave` | `tickwave convert saleae testdata/saleae/swd50-cut --channel analog_0=SWCLK --channel analog_1=SWDIO --channel digital_4_0=NRST`: 50 µs of a mixed-signal oscilloscope's analog inputs as 12-bit codes, in an 11 ms capture |
| `identify-swd.annotations.toml` | the hand-written sidecar for `identify-swd.tickwave` |
| `identify-swd.tickwavefx` | the in-memory route's fixture, cut from the same export by `scripts/extract-fixtures.ts` |
| `figure1.chart.json`, `evidence.chart.json`, `wire.chart.json` | chart data: the landing page's Figure 1, a chart whose points name annotations, and the 2026-11-04 request-clock chart |
| `crafted/` | recordings built to break a reader that trusts what a file states; its README says what each states |

Generated by `scripts/generate.ts`, not committed: `identify-swd.swd.jsonl`, the SWD decoder's
annotations of `identify-swd.tickwave` with its `recording_id`, which the TOML file includes; and
`identify-swd.bundle.tickwave`, the recording with its sidecar bundled, where the converter is
built.

`scripts/swd-annotations.ts` decodes SWD in a `.tickwave` (through the same WASM code, in Node) or a
`.tickwavefx`: line resets, the JTAG-to-SWD select, and each transfer with its request, acknowledge
and data.

### `.tickwavefx` files

The dev page's in-memory route, `src/memory-source.ts`, serves `.tickwavefx` fixtures with an
artificial delay on request (`?latency=MS`), and computes the overview at the levels a real file
has. `src/fixture-file.ts` reads them, and its opening comment gives their layout.

The dev page's stress sections read these by default. `dev/?stress=tickwave` switches them to the
same captures converted to `.tickwave`, linked into `fixtures-local/` by `scripts/fixtures.ts`.

## The elements

tickwave puts a capture into a web page with three custom elements. `<tickwave-view>` shows a whole
recording. `<tickwave-inset>` shows one annotated stretch of it, magnified. `<tickwave-ref>` turns a
phrase in the text into a reference: pointing at it opens an inset beside the text. A fourth,
`<tickwave-chart>`, in a module of its own, draws measurements against a setting.

### Loading

Include one module, and the three elements are defined:

```html
<script type="module" src="tickwave.js"></script>
```

`tickwave.js` reads recordings through a decoder in a worker. The first time a page needs a
recording, it loads `tickwave-worker.js` and `tickwave.wasm` from beside itself, so keep the three
files together. Recordings are fetched by byte range: the head first, then only the blocks a view
shows. A server that ignores ranges still works, as long as it sends the whole file.

### `<tickwave-view>`

A whole recording. Closed, it shows the recording's title and, under it, the recording's length to
three significant figures, its channels and its size, with an Open button beside them. It fetches
the recording's head, and nothing else, once it scrolls into view. Open, all of that stays, and
the button reads Close. Below it come the time axis, one row a channel, the annotations above the
rows, and a readout under the rows. The button keeps the focus, and names its recording to a screen
reader.

| Attribute | Means |
|---|---|
| `src` | the recording's URL |
| `annotations` | its sidecar's TOML file. Without it, a recording with a bundled sidecar shows that |
| `channels` | the channels to show, by name, in order. All of them by default. The view then goes as far as the furthest of them ends |
| `window` | what an open view shows first: two times (`"317.8 ms 318.0 ms"`), or an annotation's id |
| `open` | open the view |
| `series` | a series name, such as `verge`: the traces take `--series-verge`. Ink by default |

Times in attributes are a decimal with a unit (`s`, `ms`, `us`, `ns`, `ps`), or a whole number of
ticks.

The reader zooms with Ctrl or Cmd and the wheel, a trackpad pinch, or two fingers. The plain wheel
scrolls the page, and the view says how to zoom. Shift with the wheel, a sideways swipe, a drag or
the left and right arrow keys move along the capture. With the view focused, the up and down arrows
or plus and minus zoom in and out. Home and Fit show the whole capture, or all of the channels the
view shows, and Escape clears the cursors and the selection.

A mouse over a row reads it at the pointer. An analog channel gives the value of the sample
there in a label, or the smallest and largest value where one pixel covers several samples. On a
digital channel, two spans mark the pulse there: one between its edges for its width, and one over
its period to the next edge the same way. A label under the pulse gives its duty, its frequency
and 1/width, the width's reciprocal as a rate (one UART bit's gives the
baud rate). The width and the period sit on their spans once the row is tall enough for them.
Until then the label gives them too, with the pulse's level. A pulse's edges can lie far past what
the row has read, as on a quiet line zoomed in close. Once the pointer rests, the view finds them
through the overview and reads just those. Moving within the pulse lets the search go on, and once
found, the pulse reads at once anywhere in it. Where one pixel holds several edges, the label asks for
a closer look. The label keeps to the edge of the rows away from the row it reads, so it never
covers the wave there. In a plot too short for it, it runs past the plot's edge.

Each row's lower edge, in the names column, is a handle that sets the row's height. Drag it, or
focus it and press the up and down arrows (Shift for bigger steps). Home and End give the row its
least and greatest height, and Enter or a double click its default. The view grows and shrinks
with its rows. Another recording starts every row at its default height.

A click places cursor A, and the next click cursor B; with the view focused, Enter places them at
the middle of the view. A click's cursor snaps to an edge within 6 pixels on the row clicked. The
readout gives both times, the interval between them, and each analog channel's value at each.
That value is the last sample at or before the cursor, to the digit one code step carries. It also
gives each digital channel's pulse at cursor A, with the figures the pointer gives.

A drag along the time axis selects a span. The readout gives its length and each channel's edges
in it. For one channel it gives the periods' count, minimum, median and maximum, and the running
clock. The running clock is the mean of the periods inside Tukey's fences, as the sidecar's
measures define it: it keeps a clock's jitter and quantisation, and drops pauses.

A menu beside the zoom buttons steps through one kind of annotation, "3 of 16". It starts with the
kind of the annotation the view opened at, else the kind of the sidecar's first annotation.
Clicking an annotation's label shows its span.

| Method or property | Does |
|---|---|
| `show(id, span?)` | opens the view at an annotation, over `span` (`{start, end}` in ticks) if one is given. Resolves once it has drawn |
| `reveal(id, span?)` | `show(id, span)`, then brings the view into sight and puts the focus on its plot, as a reference, an inset or a chart's point does |
| `focus()` | puts the focus on the plot, or on the Open button while the view is closed |
| `setWindow(start, end)` | shows a span: ticks, or times such as `"317.8 ms"`. Called before the head arrives, it shows the span once it has |
| `settled()` | resolves once the view has drawn everything it shows |
| `tickAt(clientX)` | the tick under a point in the window, or null before the view has drawn |
| `open` | whether the view is open; setting it opens or closes it |
| `window` | the span shown, `{start, end}` in ticks |

| Event | When, and its `detail` |
|---|---|
| `tickwave-open` | the view opens: `{src}` |
| `tickwave-window` | the span shown changes: `{start, end}` |
| `tickwave-select` | a span is selected: its `start`, `end`, `duration`, `edges` by channel, and `periods` for the measured channel |
| `tickwave-ready` | the view has first drawn: `{height}` |
| `tickwave-error` | something failed: `{message, error}`. The view also shows it, and logs it to the console |

### `<tickwave-inset>`

One annotation's span, magnified, with the annotation's label and text above it. The annotations
inside the span show as notes, and an annotation's `paint` colours a line where a role drives it.
An inset sweeps in from the left when it appears. Shown again within 800 ms of hiding, it doesn't
sweep again. A reader who prefers reduced motion sees it drawn at once.

| Attribute | Means |
|---|---|
| `src`, `annotations`, `channels`, `series` | as for `<tickwave-view>` |
| `annotation` | the annotation to show, by id. Set it to switch |
| `pad` | room either side of the span: a share of it (`"10%"`, `6%` by default) or a time |
| `scale` | a fixed time scale, such as `"50 ns/px"`, so insets side by side compare |
| `sweep` | `off` to draw at once |
| `for` | the id of a `<tickwave-view>` to open at a note |
| `href` | a page to open the recording on at a note, such as tickwave's Open page |

An annotation's `measure` puts a reading under the trace, taken from the channel's own edges:

- `rate` or `period`: the running clock over the span. The note says how many periods it used,
  such as "mean of 31 of 32 periods".
- `duration`: the time from the channel's first edge in the span to its last.

A reading shows only the digits its ticks support. The key to the roles' lines sits on a line of
its own, under the reading.

A reader can point at the inset. Pointing at a note lights its span, and the line under the trace
gives the note's label, its length and who drives it, in place of the measure. That names a note
too narrow to label. Pointing at a trace reads the pulse there, as the view's label does: its level
and width, where it starts in the span, then its period, frequency and duty, as many of them as
fit on the line. On an analog channel it reads the value. Moving away puts the measure back.

A reader can zoom in within the inset's span, as close as the view goes. Ctrl or Cmd with the
wheel, a trackpad pinch, or two fingers zoom it. Zoomed in, a drag with the mouse or a finger moves
along the span, as do Shift with the wheel and a sideways swipe. A drag opens nothing. The inset
zooms out as far as its span and moves no further than its ends.
Where one pixel holds several edges, the line says how to zoom in. Shown again after 800 ms, or at
another annotation, the inset shows its whole span.

The inset is one Tab stop, named by its label. The up and down arrow keys zoom in and out, about
the lit note or else the middle. The left and right arrows move along the span. Shift with left or
right steps through the notes, and a screen reader says each. Zoomed in, the span moves to take in
the note stepped to. Escape lights none. On a touch screen, a tap lights a note or reads a pulse.

With `for` or `href`, the inset opens the recording at a note. A click on a note, Enter on the
note the keys lit, or a second tap on a lit note opens it there. A click elsewhere on the plot
opens it at the inset's own annotation. Zoomed in, the recording opens over the span the inset
shows. `for` opens a view on the page, and wins where it names one. Otherwise `href` makes the
inset a link to that page: tickwave's Open page, or any page that reads the same parameters. The
inset sets `src`, `annotations` and `annotation` in the page's query, and `window` while zoomed in:
the span shown, in whole ticks. It keeps the page's other parameters. While the whole span shows,
it drops any `window` the address had. A recording on the same site goes in by its path, and
another site's whole. As a link, the inset opens a new tab on a middle click or a Ctrl or Cmd
click, and shows its address on hover.

An inset the pointer passes through, as in a chart's popover, where `pointer-events` is `none`,
does none of this. Inside a popover the inset is no Tab stop, and the element that opened it keeps
the focus. A reference passes its arrow keys to `arrowKey(event)`, which answers them as the
inset's own and says whether it did. It reads the lit note from `lit` and the zoomed span from
`zoomed` only while its popover is open, and lights none with `clear()` as the popover closes.

An inset takes its final height once its recording's head and sidecar are in. The line under the
trace is held from then, where the annotation asks for a measure or a reader can point at the
inset, so neither the data's arrival nor the pointer moves anything below it. Before then it has
no label and no rows to size by. A page that wants nothing to move at all gives the inset its height, as
tickwave's main page does for the figure beside its name. `settled()` and the `tickwave-ready`
event (`{annotation, height}`) work as on the view, so a page can lay itself out around an inset's
final height. `sweeping` says whether the sweep is playing. `window` gives the span shown,
`{start, end}` in ticks, and `zoomed` the span the reader has zoomed to, or null while the whole
span shows, as it does again when the inset is shown after 800 ms hidden.

### `<tickwave-ref>`

A phrase that cites an annotation. Pointing at it, or reaching it with Tab, opens an inset in a
popover beside the text, so nothing on the page moves. The pointer can move into the popover to
read the inset, and has 300 ms to cross between the text and the popover. Moving the pointer or
the focus away, or Escape, closes the popover. With the focus on the reference, the arrow keys
work the inset as they do an inset with the focus. A click, Enter or Space opens its view at the
note the pointer or the keys lit, or else at the annotation, and over the span the reader zoomed the
popover to. With the popover closed, as after Escape, the view opens at the annotation's own span.
A screen reader cannot reach into the drawn inset, so the annotation's label and text describe the
reference itself.

| Attribute | Means |
|---|---|
| `annotation` | the annotation it cites |
| `for` | the id of a `<tickwave-view>` to share a recording with. The reference takes its `src`, `annotations`, `channels` and `series` |
| `src`, `annotations`, `series` | the recording, sidecar and series, for a reference without a view |

A reference fetches nothing until it scrolls into view. Then it fetches the recording's head and
the sidecar, and the inset fetches the data when it opens.

### `<tickwave-chart>`

Measurements against a setting, such as throughput against the clock asked for: a line through
each series' points. It is its own module, `tickwave-chart.js`, and loads nothing else:

```html
<script type="module" src="tickwave-chart.js"></script>

<figure>
  <tickwave-chart src="figure1.chart.json" legend="above" end-labels table="table-1"></tickwave-chart>
  <figcaption>Figure 1. 4 KiB writes against the SWD clock asked for.</figcaption>
</figure>
```

The chart draws SVG into its own children, not a shadow root, and writes its colours as
attributes. So Firefox's reader view, and a page without its CSS, still show the figure. Its
caption names it, and the table it names describes it. A reader picks a setting by pointing at it,
tapping it or stepping to it with the arrow keys, and picks a series from the legend or a line's
end label. A point can name a recording and one of its annotations. Pointing at it opens an inset
of that annotation, and a click opens the page's view of the recording there. Only then does the
chart load `tickwave.js`, from beside itself.

`CHARTS.md`, at the repository's root, specifies the data format, `tickwave-chart/1`, and every
attribute, property and event. Besides the properties below, the chart reads the line patterns
(`--dash-second-build`, `--dash-reference`, `--dash-capture-second`, `--dash-misread`),
`--capture-width`, `--capture-opacity` and `--fault-ink`.

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
| `--tickwave-<role>` | a line where an annotation's `paint` role drives it. `probe` falls back to the series colour, any other role to ink. The `target` role's lines are heavier as well, so a page without colours still tells the roles apart |
| `--font-text`, `--font-figures` | labels; the readouts' figures |

While a zoom's finer data loads, a row draws the coarser data it has. Where that data shows a
change it cannot yet place, the row draws it in the trace's colour, faded. A read still going
after 5 s puts "Still loading" in the element's notice until it lands. A read that fails puts
"Data did not load" there, with the cause, and the stretch stays faded. The row asks again when
the view next redraws, 3 s or more later, and the notice goes once the read lands.

The key under an inset names the roles it shows: "probe drives" and "target drives", and any
other role by its name. The elements follow a change of face as it happens: a `data-face`, a class
or a style on `<html>` that changes the tokens, or the system's preference. A change there that
leaves the tokens alone, such as a header marking the page scrolled, redraws nothing.

The elements take a new face in the frame after it changes. A view's and an inset's panels and
text follow the page's CSS, and their canvas redraws in that frame. A chart's colours follow the
page's CSS without a redraw.

Each element exposes its pieces to `::part()`. On the view: `summary`, `title`, `facts`,
`open-button` (Open, or Close while the view is open), `viewer`, `toolbar`, `plot`, `hint`,
`reading` (the label at the pointer), `row-handle`, `readout`, `steps`, `notice` and `button`.
Inside `facts`, each fact is a `fact` and also `fact-length`, `fact-channels` or `fact-size`, with
a `fact-separator` ("; ") between two. On the inset: `inset`, `label`, `text`, `frame`, `canvas`,
`plot` (over the canvas, what the pointer and the keys reach), `foot`, `measure`, `reading` (what
the pointer or the keys lit), `key` and `notice`. On the reference: `popover`.
A part an element hides stays hidden, whatever `display` a page's rule gives it.

### From the module

`tickwave.js` also exports these functions:

| Function | Does |
|---|---|
| `registerFile(file, sidecar)` | makes a local `File` a `src`. The sidecar's `.toml` and `.jsonl` files can come with it |
| `registerLoader(loader)` | reads recordings at the URLs `loader.accepts`, asked before the loaders registered earlier |
| `recordingInfo(src)` | what a recording holds, from its head: title, length, channels, size, content id |
| `configure({firstRead, maxDecodedBytes, cacheBytes})` | the size of the first read of a head, 32 KiB by default; the most the decoder takes from one block, overview block or head overview, 64 MiB, past which it refuses the recording; and the size of the cache of decoded blocks, 192 MiB |
