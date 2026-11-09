# tickwave charts (draft)

Draft, 2026-11-09, reviewed by the Escapement Project session. It answers that session's request
(`notes/design-2026-11-09/tickwave-charts-request.md`), which lists every chart on the landing and
measurements pages. Everything here is built; step 2's chart itself waits on the decoders writing
values.

## What a chart is

`<tickwave-chart>` draws measurements against a measured setting, such as throughput against the
clock asked for. Each series is a line through its points. A point can name the wire recording
behind it: pointing at the point opens an inset of that recording, as `<tickwave-ref>` does for a
phrase in the text.

The x values are a short list of settings, the same for every series. The y values are
measurements. A chart is not a capture: there is no time axis, no zoom and no decoder.

## Loading

`tickwave-chart.js` is its own module, published beside `tickwave.js` in each version's directory
(`/tickwave/v<semver>/`), so one version covers all four files:

```html
<script type="module" src="tickwave-chart.js"></script>
```

A page without charts never loads it. A chart never loads `tickwave.js`, the worker or the WASM
module until a reader points at a point that names a recording. Then it imports `tickwave.js` from
beside itself, and the inset loads the rest as it does on any page. A page that has already loaded
the elements keeps its own.

## Drawing

The chart draws SVG into its own children, not into a shadow root. Firefox's reader view keeps an
element's light-DOM SVG and drops any shadow root, clonable or not (tested by the Escapement
session in Firefox 160, `notes/design-2026-11-09/readertest/`).

So that the SVG reads the same in reader view, a feed or a page without its CSS:

- Data colours are resolved from the page's tokens when the chart draws, and written as
  presentation attributes. The chart redraws when the face changes.
- Text, grid, axes and neutral marks are `currentColor`.
- Fonts are presentation attributes, since reader view's sanitizer clears `style`.
- Nothing refers to an id (`url(#id)`), since reader view sets a `<base>`. The entry sweep cuts
  the lines where it has reached, rather than clipping them.

The chart draws at its box's width in CSS pixels, not into a fixed `viewBox` that scales. Text
stays at its CSS size at any width, and the chart redraws when its box changes width.

d3-scale places the points on both axes. The x axis labels the settings themselves, from
`x.values`, and skips a label that would sit within 22 px of the one before it: a tick at a value
nobody measured would mislead. The y axis takes d3's ticks, in steps of 1, 2 or 5 times a power of
ten, unless the data fixes them.

## The data: `tickwave-chart/1`

A chart reads one JSON object, from a URL (`src`) or set by the page (`data`). Every value is a
decimal string, as written by whatever measured it. `"81.0"` keeps its last digit, where a JSON
number would lose it. A readout shows a value with exactly the digits it came with, and groups the
integer part's thousands with commas: `"1238.6"` reads "1,238.6".

Figure 1 on the landing page, cut to three clocks:

```json
{
  "format": "tickwave-chart/1",
  "source": "mculink-bench results/throughput/2026-11-04: SWD, probe-rs 4 KiB writes, the median of two runs",
  "x": { "name": "SWD clock asked for", "unit": "MHz", "scale": "log", "values": ["20", "30", "50"] },
  "y": { "name": "4 KiB writes", "unit": "KiB/s", "scale": "linear", "max": "2500", "step": "500" },
  "series": [
    {
      "id": "clang", "name": "Verge, Clang build", "short": "Verge",
      "colour": "verge", "pattern": "solid", "marker": "circle",
      "points": [{ "value": "1238.6" }, { "value": "1779.8" }, { "value": "2257.8" }]
    },
    {
      "id": "stock", "name": "NXP's firmware V3.174", "short": "NXP",
      "colour": "nxp", "pattern": "solid", "marker": "circle",
      "points": [{ "value": "666.7" }, { "failed": true }, { "value": "744.6", "verdict": "wrong" }]
    }
  ],
  "limits": [{ "x": "20", "series": "stock", "label": "NXP's firmware reads right up to 20 MHz" }]
}
```


### The axes

| Field | Means |
|---|---|
| `x.values` | the settings, ascending. Every series has one point for each |
| `x.name`, `x.unit`, `y.name`, `y.unit` | the axis names; a unit is written once, at the axis |
| `x.scale`, `y.scale` | `linear` or `log` |
| `y.max`, `y.step` | a fixed top and tick step. Without them the chart picks both from the data |
| `reference` | `"y=x"` draws the diagonal, labelled "as asked" |

### A series

| Field | Means |
|---|---|
| `id` | names the series in attributes, events and other charts |
| `name` | in the legend and a readout below the plot |
| `short` | the end label, a fail mark ("GCC failed") and a tooltip readout |
| `colour` | a series token: `verge` draws in `--series-verge` |
| `pattern` | `solid`, `second-build`, `reference` or `capture-second`: the design system's line patterns (`--dash-capture-second`, the long dash of a second capture run, is still to be added) |
| `marker` | `circle`, `square`, `diamond`, `triangle-up` or `triangle-down` |
| `capture` | a run under energy capture: the line at `--capture-width` and `--capture-opacity` |
| `hidden` | drawn only once the page shows it |
| `fails` | `"mark"`: a failure takes a mark at the axis ("GCC failed"). Without it, a failure shows only as the break in the line and in the readout |

### A point

A point is `null` where the series has no figure and nothing to say about it. A line joins only
neighbouring settings that both have a value, and breaks at any other: a segment across a setting
would read as a figure there, whether the run failed, partly failed or was never made. Otherwise:

| Field | Means |
|---|---|
| `value` | the figure, or absent where none was measured |
| `verdict` | `wrong` where the firmware reads wrong at this setting: the segment into the point takes `--dash-misread`, and the point is hollow |
| `failed` | the run failed here: the line breaks, the readout says "failed", and a fail mark shows if the series marks its failures |
| `partial` | `{ "failed": 1, "of": 2, "value": "1180.5" }`: some runs failed, and the others gave this figure. The point has no `value`: the line breaks, a fail mark shows if the series marks its failures, and the readout gives this figure |
| `range` | `["1230.1", "1247.0"]`: the spread across runs, in the readout |
| `ring` | a ring around the point; with `note`, the readout says why ("from SWDIO") |
| `evidence` | `{ "recording": "...", "annotations": "...", "annotation": "..." }`: the recording, its sidecar, and the annotation that shows this point. URLs are relative to the chart's file, or to the page for data set through `data` |

### A limit

A vertical line at an x, dashed, with its `label` in the legend. With `series`, the line grows from
that series' point during the entry sweep.

## The element

### Attributes

| Attribute | Means |
|---|---|
| `src` | the chart's data |
| `show` | the series to draw, by id. Without it, every series not `hidden` |
| `selected` | an x selected from the start. With it, a selection stays until another replaces it. Without it, the selection follows the pointer and clears |
| `group` | charts with the same group share their selected x |
| `highlight` | points to ring, as `series@x`: `"clang@50 stock@20"`. A claim in the prose sets it |
| `readout` | `tooltip`, beside the selected x, or `below`, a row under the plot |
| `legend` | `above` draws a legend over the plot. Without it, the page has its own key |
| `end-labels` | name each line at its last point, with its `short` |
| `table` | the id of the page's table that gives the chart as text |
| `height` | the plot's height in CSS pixels |
| `sweep` | `off` to draw at once |

### Properties, methods and events

| Property or method | Does |
|---|---|
| `data` | the chart's data as an object. Setting it redraws: a page that slices a larger table sets each slice here |
| `pick(x, series)` | selects an x, a series or both, as a reader's pick does. `pick()` clears |
| `settled()` | resolves once the chart has drawn, its sweep included |

| Event | When, and its `detail` |
|---|---|
| `tickwave-pick` | the reader picks an x or a series: `{x, series}`, either null when cleared |
| `tickwave-evidence` | the reader opens a point's recording: `{x, series, recording, annotations, annotation}`, the URLs absolute. Cancelable (see Evidence) |
| `tickwave-ready` | the chart has first drawn: `{height}` |
| `tickwave-error` | the data did not load or does not parse: `{message, error}` |

### Reading a chart

A reader picks an x by pointing at the plot or tapping it. The nearest x is picked: each x owns the
band between the midpoints to its neighbours. The chart takes focus, and the arrow keys step through
the x values. Escape clears, and so does a tap outside the chart. A mouse leaving the plot clears,
and a finger lifting does not. With `selected`, nothing clears.

A reader picks a series from the legend or from its end label. The other series fade to 20 %. A
page picks one with `pick`, for a table column or a key.

The readout gives the picked x and each shown series' figure there, with the series' swatch: its
line and marker as drawn. A figure takes its range in brackets, its partial runs and its verdict,
such as "744.6 KiB/s, reads wrong". As a tooltip, the readout keeps off the lines it describes, and
moves left near the plot's right edge.

### Evidence

A point that names a recording behaves as `<tickwave-ref>` does. A mouse resting within 12 px of
it for 150 ms, a tap on it, or the keys reaching its setting open an inset of its annotation in a
popover beside it, so nothing on the page moves. A mouse only passing over the plot opens nothing,
so it loads nothing. The keys open the picked series' point at that setting, or else the first
shown point there that names a recording. The inset draws the probe's lines in the series' colour.

A click or Enter fires `tickwave-evidence`, which a page can cancel to take over. On a touch
screen, the tap is the click. Unless the page cancels the event, the chart opens the page's
`<tickwave-view>` of the same recording at the annotation. With no such view, the popover stays
open, and the event is all that happens.

The page decides what a point leads to, through the data it writes. A chart never guesses a
recording from a setting.

### Motion

A chart sweeps in once, when 60 % of it first shows: the lines draw from the first x to the last
over 750 ms, easing out. A limit grows from its series' point as the sweep crosses it. The end
labels then show a whole character at a time over 200 ms. A reader who prefers reduced motion, a
printed page, and a chart that already has a highlight get the end state at once. A redraw for a
face change, a resize or new data never replays the sweep.

### Accessibility

The plot is a focusable `role="group"` with `aria-roledescription="chart"`, as the view's plot is
a "capture". The page's caption names it and its `table` describes it. Each x is a stop for the
arrow keys, announced as "20 MHz". The readout is a polite live region inside the group. When a
shown point names a recording, the description adds one sentence: "Enter opens the recording
behind the picked point." An image
role would not do: its children are presentational, so the readout might never be announced. The
first build is checked in Firefox's and Chromium's accessibility trees. A page with no table gets
the readout as the only text form.

### Styling

The chart reads the tokens the other elements read (`--ink`, `--ink-2`, `--surface`, `--hair`,
`--wash`, `--series-*`, `--font-text`, `--font-figures`), and the line patterns `--dash-*`,
`--capture-width` and `--capture-opacity`. Interaction is hue-free, as on the view: an ink focus
ring and an ink wash under the pointer.

## Values on annotations

The wire-clock charts plot figures that the decoders already write into session annotations, but
only in their labels: "50 MHz asked: … read data 37.5 MHz, write data 49.9 MHz". The sidecar
format gains an optional `values` table on an annotation, each a decimal and a unit, as a string:

```toml
[[annotation]]
id = "session-50"
ref = "wire_swd/session/377858886"
values = { asked = "50 MHz", read_data = "37.5 MHz", write_data = "49.9 MHz" }
```

and in a generator's JSON Lines, `"values": {"asked": "50 MHz", ...}`. The resolver checks that
each is a decimal and a unit. This changes `tickwave-annotations/1`, so it must land before the
first recording is published and the format is frozen.

A chart still reads only chart data. mculink-bench's decoders write the values into the session
annotations (`asked`, `read_data`, `write_data` and `request`). Its `extract.py` reads the sidecars
and writes the chart's data, with each point's evidence, so the page keeps the choice of what a
point leads to.

## What stays with the page

- The landing page's dial: the project's identity, with motion of its own.
- Tables, such as Table 1: page HTML, so they read without scripts. A chart names its table and
  follows it through `pick` and `tickwave-pick`.
- The headline and its sentences.
- The case maps and the stall marks: no settings axis, no evidence.
- The key, the set picker, the wire selector and the matrix's controls. They set `show` and
  `data` on the charts.

## Steps

1. The landing page's Figure 1. Two series and a third on demand, the limit with its legend entry,
   the tooltip readout, pointer, touch and keys, the links to Table 1 and the claim, the sweep,
   reader view and both faces. Its points name no recording: the 4 KiB writes were not captured.
2. The 2026-11-04 wire-clock chart, from the session annotations' values. Every point opens its
   recording. This needs `values` in the sidecar format first.
3. The measurements page's line charts, with the page's key and the command matrix's slices.
