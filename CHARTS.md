# tickwave charts

`<tickwave-chart>` draws measurements against a setting, such as throughput against the clock
asked for. This document specifies the element and its data, `tickwave-chart/1`.

Status: draft, with the recording format.

## What a chart is

Each series is a line through its points. A point can name the wire recording behind it: pointing
at the point opens an inset of that recording, as `<tickwave-ref>` does for a phrase in the text.

The x values are a short list of settings, the same for every series. The y values are
measurements. A chart is not a capture: it has no time axis, no zoom and no decoder.

## Loading

`tickwave-chart.js` is its own module. It is published beside `tickwave.js` in each version's
directory (`/tickwave/v<semver>/`), so one version covers all four files:

```html
<script type="module" src="tickwave-chart.js"></script>
```

A page without charts never loads it. A chart loads nothing else until a reader points at a point
that names a recording. Then it imports `tickwave.js` from beside itself, and the inset loads the
worker and the WASM module as it does on any page. A page that has already loaded the elements
keeps its own.

## Drawing

The chart draws SVG into its own children, not into a shadow root. Firefox's reader view keeps an
element's SVG children and drops any shadow root, clonable or not (Firefox 160).

The SVG reads the same in reader view, in a feed, or on a page without its CSS:

- Data colours come from the page's tokens when the chart draws, and go into the SVG as
  presentation attributes. Each also has a style that refers to its token, so where the page's CSS
  applies, the colours follow the face without a redraw. Where the page fades its colours as the
  face turns, they fade with it. Reader view clears the style and keeps the attributes.
- Text, grid, axes and neutral marks are `currentColor`.
- Fonts are presentation attributes, since reader view's sanitizer clears `style`.
- Nothing refers to an id (`url(#id)`), since reader view sets a `<base>`. So the entry sweep cuts
  the lines where it has reached, rather than clipping them.

The chart draws at its box's width in CSS pixels, not into a fixed `viewBox` that scales. Text
stays at its CSS size at any width, and the chart redraws when its box changes width.

d3-scale places the points on both axes. The x axis labels the settings themselves, from
`x.values`. It leaves out a label that would sit within 22 px of the one before it: a tick at a
value nobody measured would mislead. The y axis takes d3's ticks, in steps of 1, 2 or 5 times a
power of ten, unless the data fixes them. The plot's left margin makes room for the widest y label
and for the unit above them.

## The data: `tickwave-chart/1`

A chart reads one JSON object, from a URL (`src`) or from the page (`data`). Every value is a
decimal string, as whatever measured it wrote it. `"81.0"` keeps its last digit, where a JSON
number would lose it. A readout shows a value with exactly the digits it came with, and groups the
whole part's thousands with commas: `"1238.6"` reads "1,238.6".

The landing page's Figure 1, cut to three clocks:

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
      "points": [
        { "value": "666.7" },
        { "failed": true },
        { "partial": { "failed": 1, "of": 2, "value": "744.6" }, "verdict": "wrong" }
      ]
    }
  ],
  "limits": [{ "x": "20", "series": "stock", "label": "NXP's firmware reads right up to 20 MHz" }]
}
```

### The axes

| Field | Means |
|---|---|
| `x.values` | the settings, ascending. Every series has one point for each |
| `x.min`, `x.max` | the axis' ends, where it reaches past the first and last settings: `"0.8"` and `"62.5"` keep points at 1 and 50 MHz off the plot's edges |
| `x.name`, `x.unit`, `y.name`, `y.unit` | the axes' names. A unit is written once, at its axis |
| `x.scale`, `y.scale` | `linear` or `log` |
| `y.max`, `y.step` | a fixed top and tick step. Without them, the chart picks both from the data |
| `reference` | `"y=x"` draws the diagonal, named "as asked" in the legend |

### A series

| Field | Means |
|---|---|
| `id` | names the series in attributes, events and other charts: letters, digits, `.`, `_` and `-` |
| `name` | in the legend and in a readout below the plot |
| `short` | in the end label, a fail mark ("GCC failed") and a tooltip readout |
| `colour` | a series token: `verge` draws in `--series-verge` |
| `pattern` | `solid`, `second-build`, `reference` or `capture-second`: the design system's line patterns |
| `marker` | `circle`, `square`, `diamond`, `triangle-up` or `triangle-down` |
| `capture` | a run under energy capture: the line takes `--capture-width` and `--capture-opacity` |
| `hidden` | drawn only once the page shows it |
| `fails` | `"mark"`: a failure takes a mark at the axis ("GCC failed"). Without it, a failure shows only as the break in the line and in the readout |

### A point

A point is `null` where the series has no figure and nothing to say about it. A line joins only
neighbouring settings that both have a value, and breaks at any other. A segment across a setting
would read as a figure there, whether the run failed, partly failed or never happened.

| Field | Means |
|---|---|
| `value` | the figure, or absent where none was measured |
| `verdict` | `wrong` where the firmware reads wrong at this setting: the segment into the point takes `--dash-misread`, and the point is hollow |
| `failed` | the run failed here: the line breaks, the readout says "failed", and a fail mark shows if the series marks its failures |
| `partial` | `{ "failed": 1, "of": 2, "value": "1180.5" }`: some runs failed, and the others gave this figure. The point has no `value`. The line breaks, a fail mark shows if the series marks its failures, and the readout gives this figure |
| `range` | `["1230.1", "1247.0"]`: the spread across runs, in the readout |
| `ring` | a ring around the point |
| `note` | a few words the readout adds after the figure: "18.4 MHz, the read failed partway" |
| `evidence` | `{ "recording": "...", "annotations": "...", "annotation": "..." }`: the recording, its sidecar, and the annotation that shows this point. URLs are relative to the chart's file, or to the page for data set through `data`. A failed or partial point's recording opens from its fail mark, so only in a series that marks its failures |

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
| `highlight` | points to ring, as `series@x`, and `limits` to strengthen the limit lines as their legend entry does: `"clang@50 stock@20 limits"`. A claim in the prose sets it |
| `readout` | `tooltip`, beside the selected x, or `below`, a row under the plot |
| `legend` | `above` draws a legend over the plot. Without it, the page has its own key |
| `end-labels` | names each line at its last point, with its `short`. Labels that would overlap move apart, and the plot leaves room for the widest |
| `table` | the id of the page's table that gives the chart as text |
| `height` | the plot's height in CSS pixels |
| `sweep` | `off` draws at once |

### Properties, methods and events

| Property or method | Does |
|---|---|
| `data` | the chart's data as an object. Setting it redraws: a page that slices a larger table sets each slice here. The reader's picked x stays picked where the new data has it. Reading it gives the parsed data, not the object set, so a page that skips unchanged data keeps track of what it set |
| `pick(x, series)` | selects an x, a series or both, as a reader's pick does, and moves the group with it. It fires no event. `pick()` clears |
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
line and marker as drawn. A figure takes its range in brackets, its partial runs, its note and its
verdict, such as "failed in 1 of 2 runs (744.6 KiB/s in the other), reads wrong". As a tooltip,
the readout sits beside the picked x, or to its left near the plot's right edge. It sits at the
plot's top, or lower where that hides less of the lines. A long line wraps.

On a chart too narrow for some setting's readout to fit beside it, as on a phone, the tooltip
floats off the chart for every setting. It sits below the chart at the picked x, or above it where
the window has no room below. There it covers the page around the chart, and none of the chart.

### Evidence

A point that names a recording behaves as `<tickwave-ref>` does. An inset of its annotation opens
in a popover when a mouse rests within 12 px of the point for 150 ms, when a finger taps it, or
when the keys reach its setting. The popover opens below the chart, or above it when there is no
room below, across the point's x. Nothing on the page moves, and the popover covers none of the
chart, nor a floating readout: the picked x's rule joins it to the point. Its first line names the series, and the inset
draws the probe's lines in the series' colour. A failure's mark opens its point's recording the
same way.

A mouse only passing over the plot opens nothing, so it loads nothing. Where points lie on one
another, the picked series' point opens, or else the first series' in the data. The keys open the
picked series' point at that setting, or else the first there that names a recording.

A click or Enter fires `tickwave-evidence`. A page can cancel it and take over. On a touch screen,
the tap is the click. Unless the page cancels the event, the chart opens the page's
`<tickwave-view>` of the same recording at the annotation. With no such view, the popover stays
open, and the event is all that happens.

The page decides what a point leads to, through the data it writes. A chart never guesses a
recording from a setting.

### Motion

A chart sweeps in once, when 60 % of it first shows, or 60 % of the window for a chart taller than
the window. The lines draw from the first x to the last over 750 ms, easing out. A limit grows from
its series' point as the sweep crosses it. The end labels then appear a whole character at a time
over 200 ms. A reader who prefers reduced motion, a printed page, and a chart that already has a
highlight get the end state at once, and a highlight set while the sweep plays ends it. A face change
does not redraw the chart, and a redraw for a resize or new data never plays the sweep again.

### Accessibility

The plot is a focusable `role="group"` with `aria-roledescription="chart"`, as the view's plot is a
"capture". The page's caption names it, and its `table` describes it. Each x is a stop for the
arrow keys, announced as "20 MHz". The readout is a polite live region inside the group. When a
shown point names a recording, the description adds one sentence: "Enter opens the recording
behind the picked point."

An image role would not do: its children are presentational, so the readout might never be
announced. The tests check the tree in Chromium; Firefox's tree was checked by hand. A page with no
table has the readout as the chart's only text form.

### Styling

The chart reads the tokens the other elements read (`--ink`, `--ink-2`, `--surface`, `--hair`,
`--wash`, `--series-*`, `--font-text`, `--font-figures`), and the line patterns `--dash-*`,
`--capture-width` and `--capture-opacity`. Interaction is hue-free, as on the view: an ink focus
ring and an ink wash under the pointer.

## Where a chart's figures come from

A chart reads only chart data. The wire-clock charts plot figures that the decoders write into
their session annotations as `values` (ANNOTATIONS.md): `asked`, `read_data`, `write_data` and
`request`. mculink-bench's `extract.py` reads the sidecars and writes the chart's data, with each
point's evidence. So the page keeps the choice of what a point leads to.
