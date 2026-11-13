# escapement-tickwave-egui

tickwave's native renderer: a recording drawn with egui, through the view core. A `Graph` draws
one analog trace with digital lanes under it on the same time axis, and a `Strip` draws the whole
recording small under the graph. Both draw what a `Viewer` answers.

## The viewer

`Viewer::open` opens a finished recording at a path. `Viewer::live` opens one a writer in the
same process is still writing, from the writer's snapshot, and `Viewer::update` takes each newer
snapshot. When the writer finishes the file, or a compacted copy replaces it, `Viewer::reopen`
opens the file at the path again, finished; the view stays as it was.

Queries run on a thread of their own. A newer query replaces one that hasn't started and cancels
one that has, so only the newest answer is kept, and its arrival asks egui for a frame. A graph
following a recording as it is written draws every frame. It asks again as soon as its last
answer has landed and either a newer snapshot has come or the window has moved past what that
answer covers; there is no timer. An answer covers its window and a width either side, so a
moving now alone asks nothing: the graph draws the answer it holds, moved to the window on
screen, and the trace scrolls every frame.
`Viewer::answer` gives the latest answer, with the window's and the selection's statistics for a
host's panel. `Viewer::failure` says why the last query or snapshot failed. `Viewer::recording`
reads the recording itself, for what it says of the whole capture: samples lost, gaps, sources
and markers.

## The view the host keeps

A `ViewState` holds the window, the value range, the selection and whether a drag selects. The
window follows the recording's end, or stays where it was put. The value range fits what the
window shows, or stays where it was moved. The graph and the strip change the state as the
reader moves them, and the host can change it too: a pan, a zoom or a selection gives the window
a place of its own, and setting it to follow again resumes following.

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
- Each digital lane's levels as lines, a column with more than one edge filled, and its gaps
  shaded.
- Marker lines, with their labels in up to three rows across the plot's top. A fitted value
  range keeps those rows clear once the recording has a marker.
- The selection, washed across the plot and the lanes.
- The time axis and two value labels, with the digits their source carries and no more, both
  value labels in one unit.

While a query runs, the graph draws the latest answer moved to the window on screen. An answer
covers the window and a width either side, so a pan draws at once and a zoom stretches what is
held. Past that, the trace comes from the strip's columns, so nothing goes blank. A drag never
waits for a query.

## Gestures

Drag or scroll to move along time; ctrl-scroll or pinch to zoom it. A vertical drag or scroll
past 12 points takes the value range off the fit and moves it; a drag along the value axis zooms
it; a double click fits it again. With `select_mode`, a drag selects.

The strip has a time axis along its foot. A click or a drag on it centres the window. A handle
at each end of the window drags that end, at least one tick short of the other.

## The palette

Every colour, font and opacity comes from the host's `Palette`; the crate has none of its own.
verge fills it from its design tokens. The demo reads the project's tokens from
`site/tokens.css`, in the day and the night face.

## Spans

A frame's work is in `tracing` spans at debug level: `graph`, and in it `layout`,
`build query`, `axes`, `trace`, `lane`, `markers` and `tessellate`, and `strip`. The query
thread's are `query job` and `snapshot`, around the view core's own.

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
finished. `--trace FILE` writes every span as a Chrome trace, for Perfetto
or chrome://tracing.

`tests/shots.rs` renders the graph and the strip to PNG files through egui_kittest's wgpu
renderer, in both faces: set `TICKWAVE_SHOTS` to a folder and run it with `--ignored`.
