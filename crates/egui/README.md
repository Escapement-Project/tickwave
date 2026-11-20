# escapement-tickwave-egui

tickwave's native renderer: a recording drawn with egui, through the view core. A `Graph` draws
one analog trace with digital lanes under it on the same time axis, or, made with
`Graph::lanes_only`, digital lanes alone, and a `Strip` draws the whole recording small under the
graph. Both draw what a `Viewer` answers.

## The viewer

`Viewer::open` opens a finished recording at a path. `Viewer::live` opens one a writer in the
same process is still writing, from the writer's snapshot, and `Viewer::update` takes each newer
snapshot. When the writer finishes the file, or a compacted copy replaces it, `Viewer::reopen`
opens the file at the path again, finished; the view stays as it was. A `Viewer` is `Send` and
`Sync`, so a host may open it off the UI thread and hand it over, or call it from its file thread.

A viewer keeps decoded data in a cache: index pages, blocks and overview blocks, the least recently
used let go of first. It holds about 256 MiB unless `ViewerOptions::cache_bytes`, given to
`Viewer::open_with` or `Viewer::live_with`, says otherwise, and `reopen` keeps that budget. A
recording followed as it is written fills it with the tail's blocks, each decoded as the next
answer reads it, until it reaches its budget: about 160 seconds into a verge capture at 100,000
pairs a second, with the default.

A compaction renames a copy over the file, and Windows refuses that while the file is open.
`Viewer::release` lets go of the file first: it cancels the query under way and runs none until
`reopen`, while the graph draws the answer it holds, moved to the window on screen. What
`Viewer::recording` reads of the file fails meanwhile. A `reopen` that finds the file still being
written returns `Error::Unfinished`, and a live viewer let go of reads the file again and carries
on live.

Queries run on a thread of their own. A newer query replaces one that hasn't started and cancels
one that has, so only the newest answer is kept, and its arrival asks egui for a frame. Until the
first answer arrives, the graph and the strip draw a sketch of the first query, from each
channel's coarsest level: a finished file's footer holds it, so the first frame waits on nothing
past the footer. The graph keeps room for the marker labels from that frame on whenever the
recording has markers, read or not. A graph
following a recording as it is written draws every frame. After each snapshot the viewer answers
the followed request again itself, at once: it keeps the columns of the answer it holds before
the first tick the snapshot changed, and reads only from there, its window ending at the now the
graph gave it last. The graph asks again only when what it asks changes, or when the window has
moved past what the answer covers; there is no timer. A change of the channels drawn is asked at
once. Any other change waits only for a request of the graph's own under way, so a pan draws its
answers as they come rather than cancelling each one. It cancels a snapshot's follow-up, which
the next snapshot runs again. An answer covers its window and at least a width either side, so a
moving now alone asks nothing: the graph draws the answer it holds, moved to the window on
screen, and the trace scrolls every frame. A followed window's columns sit at fixed ticks, so its
answer's window ends a whole column on, and now scrolls the picture between columns.
`Viewer::answer` gives the latest answer, with the window's and the selection's statistics for a
host's panel. `Viewer::busy` says a request or a snapshot is waiting, a snapshot is being taken,
or a query runs, a follow-up included, so the answer drawn may be older than the view. Once it is
false, a followed window's answer is of the last snapshot given, unless `Viewer::failure` says
why not. `Viewer::failure` says why the last query or snapshot failed. `Viewer::recording` reads
the recording itself, for what it says of the whole capture: samples lost, gaps, sources and
markers.

## A recording being written

What a host does for a viewer of a recording its writer is still writing:

- **One snapshot a frame.** Give `Viewer::update` the writer's newest snapshot at most once a
  frame, taken on the UI thread. The viewer keeps only the newest it was given, but each one taken
  costs the query thread its update and follow-up: given every millisecond at 100,000 pairs a
  second, they keep it busy almost all the time.
- **Bytes first.** Give a snapshot only once the bytes it covers are in the file.
- **One viewer a capture.** Open a new `Viewer::live` for each capture. A snapshot of another
  writer is the `failure`.
- **A finite span.** A followed window with a finite span is that span wide from the capture's
  start: its part before the capture is empty, and the data enters from the right. Its width
  stays the same, so each snapshot's follow-up extends the answer held. The whole recording
  followed, `Follow { span: i64::MAX }`, starts at the recording's start and widens with each
  snapshot, so each follow-up is a whole query.
- **Now.** Each frame while recording, give `Graph::now` in the recording's ticks: the newest
  snapshot's data end plus the time since it was published, kept a little ahead of the data at
  most. From the writer's finish on, give None: a now left set moves a followed window past the
  recording's end.
- **Busy.** Once `busy` is false, a followed window's answer is of the last snapshot given, or
  failed. A host that waits for the answer to a snapshot waits for `busy` to turn false.
- **The window's statistics.** A snapshot's follow-up ends its window at the now the graph gave the
  frame before, so `statistics[0]` is of the window drawn then, a frame behind at most.
- **The finish.** After the writer's finish, `reopen`. A snapshot given after that is one taken
  before the finish, and is dropped. `release` can come before the finish: a `reopen` then finds
  the file unfinished, returns `Error::Unfinished`, and the viewer carries on live.
- **A copy over the file.** To replace the finished file with a compacted copy, write the copy
  beside it while the viewer reads the original, then `release`, rename the copy over the file,
  and `reopen`. Windows refuses the rename while the viewer holds the file.

## The view the host keeps

A `ViewState` holds the window, the value range, the selection and whether a drag selects. The
window follows the recording's end, or stays where it was put. The value range fits what the
window shows, or stays where it was moved. The graph and the strip change the state as the
reader moves them, and the host can change it too: a pan, a zoom or a selection gives the window
a place of its own, and setting it to follow again resumes following. A zoom while following a
recording being written keeps following, at the new span, about the end; a selection drag stops
following, so the span marked stays where it was drawn.

`Graph::shown` reads, from the graph's response, the window and the value range it drew last. A
host fixes Y where it is on screen by setting `Y::Fixed` from that range.

A host builds a `ViewState` from `ViewState::default()`, the whole recording followed, and sets
the fields it wants on that value; the `Extras` a graph adds to its query for a host's own panel
come the same way, from `Extras::default()`. The view core's `Query` starts from
`Query::new(window, columns)` and its `MarkerQuery` from `MarkerQuery::default()`. None of the
four is built as a literal outside its crate, so a later version can add a field, and a host
matching on the view core's `Error` keeps an arm for errors added later.

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
- Where an annotation paints a channel (ANNOTATIONS.md, "Roles"), that stretch of the trace or
  the lane in the role's look: its colour, its weight added to the line weight, and its dash, in
  line widths. A dash starts where its stretch starts, and again after a gap in the data. The
  latest paint in sidecar order draws a column, and a paint that names no declared role draws as
  none. Every paint over the answer's span draws, however many markers the window holds.
- Under the time axis, the key: every role the recording declares, in its order, right-aligned,
  each a short line drawn as the role's lines are, then its label. Its room is kept whenever the
  recording declares a role, so the plot keeps its height as the reader moves. A recording that
  declares none has no key and no room for one.
- At the time axis's right end, while the recording is being written, a Live toggle: filled in
  ink while the window follows, outlined while it doesn't, in one place and one size either way.
  A press, or Space or Enter while it has the focus, stops following at the window drawn, or
  follows again at its span. It shows a followed window the host sets from the next frame, and
  makes the response `changed()` when it toggles. AccessKit reads it as a toggled button.
- Near a marker's line, over the plot and the lanes, the pointer reads the marker: its time at
  the recording's tick, then its number and label.

A dash whose period comes to less than a device pixel, as a tiny `line_weight` makes it, draws
solid.

A graph from `Graph::lanes_only` draws its lanes, a band above them for the markers' labels where
the recording has markers, and the time axis: no trace, no value axis, and no flag. Its height is
what those take unless the host gives one. It has no strip either: the strip is the trace's
overview, so a view of digital channels alone has none.

While a query runs, the graph draws the latest answer moved to the window on screen. An answer
covers the window and a width either side, so a pan draws at once and a zoom stretches what is
held. Past that, the trace comes from the strip's columns and each lane from its own columns over
the extent, which the graph asks for with every query, so no column goes blank. A column of those
that the answer's span cuts draws the levels of its part outside the span, read again, not the
whole column's. A drag never waits for a query.

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

`trace` is the trace's colour and the lanes'. None, as `from_visuals` leaves it, takes the colour
the recording stores (ANNOTATIONS.md, "The stored view") on the face `dark` names, else `ink`.
`from_visuals` sets `dark` from the theme. `roles` holds the host's own look for a role, by its
name: a `view::roles::Host` whose colour, weight and dash each win over the role's declaration,
as a page's `--tickwave-role-<name>` properties do. A weight outside 0 to 4, or a dash outside the
bounds a declaration's is held to, is left to the declaration, and `Dash::Solid` draws a dashed
role solid. A role's colour that neither gives comes from its place among the declared roles: the
first takes the trace's, the rest `ink`.

## The stored view

A recording's sidecar may store the channels a view shows and the span it opens at. The graph
draws the channels and the window its host gives it, so applying them is the host's:
`Viewer::recording` reads them with `Recording::stored_view`. The trace's stored colour is the
graph's own, through `Palette::trace`. The demo applies the stored view: the first analog
channel it names is the trace and its digital ones the lanes, in its order, and its span is the
window where `--window` gives none.

## Spans

A frame's work is in `tracing` spans at debug level: `graph`, and in it `layout`,
`build query`, `axes`, `trace`, `lane`, `markers` and `tessellate`, and `strip`. The query
thread's are `sketch job`, `query job` and `snapshot`, around the view core's own: `sketch`,
`query`, and `extend` where a snapshot's answer extends the one held, followed by `query` where
it can't.

## Decoding

With the `decode` feature, a graph draws protocol decoders' items as tracks under it, one track an
analysis, on the graph's time axis. `escapement-tickwave-decode` runs the decoders, and
`escapement-tickwave-decoders` holds the reference ones and their registry.

`Decode::start` takes a recipe, the registry to bind it through, and a function that opens the
recording. It returns at once: a worker opens the recording, binds the recipe and runs the forward
pass. `Decode::progress` says where that stands, or why the recipe didn't bind. The pass keeps no
items. It tallies each analysis's items as they close, and a track draws that tally zoomed out,
as how many items start in each column. Zoomed in, the window on screen and a width either side
are decoded again from the nearest checkpoint before them, and the tracks and the panel read
those items. A window the pass hasn't reached yet is decoded from its own start, and draws pale
with dashed outlines until the pass gets there. Checkpoints take at most 64 MiB, unless
`DecodeOptions` says otherwise.

`Graph::decoded` draws a graph with a decode's tracks under it, from a `DecodeState` the host
keeps as it keeps the `ViewState`. `DecodedGraph::panel` places the side panel beside the plot,
or under it where the plot would be left too narrow. `Decode::release` and `Decode::reopen` let
go of the file and open it again, as the viewer's do.

A track keeps its rows' height from the first frame. Its gutter names the analysis and each row.
Lost input is shaded as the graph shades lost time, input not decoded yet is hatched, and decoded
input with nothing in it is left bare. An item closed incomplete, or holding a value the decoder
couldn't know, is marked at every zoom. The pointer resting on a box shows its fullest label,
start and length, and a click selects it, outlined in ink.

The side panel has four tabs:

- **Selected**: the selected item in full: its label, time, duration and status, its fields, its
  spanned fields as sections that open to their bits, the values it couldn't know and why, its
  links, and the items stacked on it that read it. Previous and next step through its kind, and
  an item off screen comes into view, keeping the window's span. Its citation, or the tab as
  text, copies to the clipboard.
- **List**: an analysis's items as rows in time order, filtered by kind or to those holding a
  condition. A click selects one.
- **Text**: the bytes of an analysis whose schema declares a kind's text, joined into lines.
- **Decoders**: each analysis under the one it reads, with its inputs, its settings as controls,
  and how far its pass has come. A setting changed decodes that analysis's chain again, and the
  window on screen at once, provisional until the pass reaches it. Below them, a decoder can be
  added: one reading the recording's channels, bound to a channel per port, one reading an
  analysis already there, or a preset stack. An analysis can be removed, with what reads it.

`Decode::recipe` gives the recipe as the reader has changed it, for a host that saves it.

## The demo

```sh
cargo run --release -p escapement-tickwave-egui --features decode --example demo -- web/fixtures/energy.tickwave
cargo run --release -p escapement-tickwave-egui --features decode --example demo -- --live 100000
```

A recording opens as its stored view says (above), its trace in its stored colour, else ink.
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

`--recipe FILE` decodes the recording as the file's `[[decode]]` tables say, with the side panel
beside the plot. `--decode` opens it with nothing decoded yet, the Decoders tab open to add some.
The decoders' recipes are in `crates/decoders/recipes/`:

```sh
cargo run --release -p escapement-tickwave-egui --features decode --example demo -- capture.tickwave --recipe crates/decoders/recipes/swd.toml
```

`tests/shots.rs` renders the graph and the strip to PNG files through egui_kittest's wgpu
renderer, in both faces: set `TICKWAVE_SHOTS` to a folder and run it with `--ignored`.
