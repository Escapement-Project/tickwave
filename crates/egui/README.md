# escapement-tickwave-egui

tickwave's native renderer: a recording drawn with egui, through the view core. A `Graph` draws
one analog trace with digital lanes under it on the same time axis, or, made with
`Graph::lanes_only`, digital lanes alone, and a `Strip` draws the whole recording small under the
graph. Both draw what a `Viewer` answers.

## The viewer

`Viewer::open` opens a finished recording at a path. `Viewer::live` opens one a writer in the
same process is still writing, from the writer's snapshot, and `Viewer::update` takes each newer
snapshot. When the writer finishes the file, or a compacted copy replaces it, `Viewer::reopen`
opens the file at the path again, finished; the view stays as it was.

Queries run on a thread of their own. A newer query replaces one that hasn't started and cancels
one that has, so only the newest answer is kept, and its arrival asks egui for a frame. Until the
first answer arrives, the graph and the strip draw a sketch of the first query, from each
channel's coarsest level: a finished file's footer holds it, so the first frame waits on nothing
past the footer. The graph keeps room for the marker labels from that frame on whenever the
recording has markers, read or not. A graph
following a recording as it is written draws every frame. After each snapshot the viewer answers
the followed request again itself, at once: it keeps the columns of the answer it holds before
the first tick the snapshot changed, and reads only from there. The graph asks again only when
what it asks changes, or when the window has moved past what the answer covers; there is no
timer. An answer covers its window and at least a width either side, so a moving now alone asks
nothing: the graph draws the answer it holds, moved to the window on screen, and the trace
scrolls every frame. A followed window's columns sit at fixed ticks, so its answer's window ends
a whole column on, and now scrolls the picture between columns.
`Viewer::answer` gives the latest answer, with the window's and the selection's statistics for a
host's panel. `Viewer::failure` says why the last query or snapshot failed. `Viewer::recording`
reads the recording itself, for what it says of the whole capture: samples lost, gaps, sources
and markers.

## The view the host keeps

A `ViewState` holds the window, the value range, the selection and whether a drag selects. The
window follows the recording's end, or stays where it was put. The value range fits what the
window shows, or stays where it was moved. The graph and the strip change the state as the
reader moves them, and the host can change it too: a pan, a zoom or a selection gives the window
a place of its own, and setting it to follow again resumes following. A zoom while following a
recording being written keeps following, at the new span, about the end.

While a recording is being written, the host passes its now, in ticks, to the graph and the
strip. A followed window ends at the later of now and the data's end. Nothing is drawn between
the data's end and now: the trace and the lanes stop where the data does. A finished recording
takes no now.

## What the graph draws

- The trace, a band from each column's least to its greatest value; zoomed in far enough, each
  sample, held for its time, with a dot once the samples stand apart.
- A column that touches lost time draws pale, and so does a sample where the flag channel (such
  as `under_range`) is high. Lost time is shaded at least a pixel wide, so a loss stays visible
  at every zoom. A pause only breaks the trace.
- Each digital lane's levels as lines, and its gaps shaded. Where edges crowd closer than a
  device pixel, the band between the levels shades by the edge rate, in edges a second, on one
  scale at every zoom and in every recording: 20 % at one edge a second or fewer, 60 % at 100
  million and above, linear in the rate's logarithm between. Each level's line takes the band's
  opacity times its share of time over five columns divided by the larger share, at least a
  fifth of the band's where those hold an edge, so a rare excursion stays visible; with no edge
  in them, the band's opacity times its share. A short pause between two runs, under the join
  distance (a line's width and a device pixel), keeps its gap, and its line takes the larger of
  the two runs' bands. An edge standing apart is a line across the band. So is a busy column, or
  a run of edges whose first and last are under the join distance apart, with no other edge
  within a device pixel: one line at its middle, so a short pulse reads alike wherever the
  columns fall. A line with neighbours near is faded towards the shade their spacing gives; a
  pulse counts no pulse as a neighbour, so pulses near each other stay full ink. The view core's
  `shade` module holds the model, which the web's drawing ports.
- Marker lines, with their labels in up to three rows across the plot's top. A fitted value
  range keeps those rows clear once the recording has a marker.
- The selection, washed across the plot and the lanes.
- The time axis and two value labels, with the digits their source carries and no more, both
  value labels in one unit.

A graph from `Graph::lanes_only` draws its lanes, a band above them for the markers' labels where
the recording has markers, and the time axis: no trace, no value axis, and no flag. Its height is
what those take unless the host gives one.

While a query runs, the graph draws the latest answer moved to the window on screen. An answer
covers the window and a width either side, so a pan draws at once and a zoom stretches what is
held. Past that, the trace comes from the strip's columns, so nothing goes blank. A drag never
waits for a query.

## Gestures

Drag or scroll to move along time; ctrl-scroll or pinch to zoom it. A vertical drag or scroll
past 12 points takes the value range off the fit and moves it; a drag along the value axis zooms
it; a double click fits it again. A graph of lanes alone has no value range to move. With
`select_mode`, a drag selects.

A zoom step sets the host's window to where the view is going at once, so the response is
`changed()` once a step, and the graph eases the window it draws there over 150 ms, quick at
first. The step's query goes out in that frame, by the usual rule, and until its answer lands the
graph draws what it holds mapped onto the window drawn, as for any query. A step during an ease
goes on from where the last one was going, from the window drawn. A pan during an ease moves on
from where it was going, at once. The graph eases only toward the window it set itself: a window
the host sets is drawn at once, and a host that states the same window every frame, as one
following a recording might, stops nothing. A host whose reader asked for less motion passes
`Graph::motion(false)`, and each step jumps; the graph doesn't read the platform's setting.

The strip has a time axis along its foot. A click or a drag on it centres the window. A handle
at each end of the window drags that end, at least one tick short of the other.

## The palette

Every colour, font, opacity and line weight comes from the host's `Palette`; the crate has none
of its own. A host starts from `Palette::from_visuals`, which takes the colours from its egui
theme, as the web's elements take the system's colours on a page without tokens, and the web's
opacities and line weight, then sets what its own tokens give. A `Palette` is built no other way:
a later version may add a field. `line_weight`, in points, draws the trace's lines and the lanes'
levels and edges, 1.5 from `from_visuals`. verge fills it from its design tokens. The demo reads
the project's tokens from `site/tokens.css`, in the day and the night face.

## Spans

A frame's work is in `tracing` spans at debug level: `graph`, and in it `layout`,
`build query`, `axes`, `trace`, `lane`, `markers` and `tessellate`, and `strip`. The query
thread's are `sketch job`, `query job` and `snapshot`, around the view core's own: `sketch`,
`query`, and `extend` where a snapshot's answer extends the one held, followed by `query` where
it can't.

## The demo

```sh
cargo run --release -p escapement-tickwave-egui --example demo -- web/fixtures/energy.tickwave
cargo run --release -p escapement-tickwave-egui --example demo -- --live 100000
```

`--live N` records a synthetic capture at N pairs a second, laid out as verge's energy captures
are, and shows it as it is written. It writes a batch of pairs every 10 ms, as an instrument's
messages arrive, and hands the viewer a snapshot at most once a 16 ms frame. The now it passes
to the graph is the newest snapshot's data end plus the time since that snapshot, at most 50 ms
past the data. With `--seconds S` the recording finishes after S seconds and the demo reopens it
finished; with `--from S` it starts S seconds in, those written as fast as they can be before the
window shows them. `--trace FILE` writes every span as a Chrome trace, for Perfetto or
chrome://tracing, and `--quit-after S` closes the window S seconds after its first frame. For
measuring a frame, `--window FROM..TO` opens at a window, in seconds, and `--repaint` draws every
frame though nothing changes.

`tests/shots.rs` renders the graph and the strip to PNG files through egui_kittest's wgpu
renderer, in both faces: set `TICKWAVE_SHOTS` to a folder and run it with `--ignored`.
