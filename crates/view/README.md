# escapement-tickwave-view

The viewer's logic apart from the drawing, for a native renderer and later the web. It opens a
recording, finished or still being written, and answers what a view asks of it: each channel's
pixel columns, single samples once there are few enough, the overview strip, statistics over an
interval, and the markers. It also has the web viewer's readouts with exact digits, times, value
scale, time axis and point probes.

## A recording and a query

`Recording::open` reads a finished file's trailer and footer, from any `Bytes`: a file, or bytes in
memory. Nothing else is read until a query asks for it. A channel's strip is decoded when it is
first drawn. A query reads only the index pages that place the blocks and overview blocks its
span covers, directory pages included. Where a channel has more than 64 gaps or breaks, its list
is in pages too: a query, statistics, `edges` and `index_at` read only the list pages their span
needs, and each block a query decodes is checked against the entries its own ticks need.
`runs` and `known` read the whole list. A finished file's markers are read a range at a time:
`markers` reads the annotation parts its range meets and the bundle's hand-written file, and the
extent takes their span from the parts table, no part read. A hand-written `ref` shows once every
part is read, as `load_markers`, `marker_count`, `markers_left_out` and a query's marker list
read them; `load_markers` says why they don't read, where they don't. A channel this version can't
read keeps its place and name as `ChannelKind::Unavailable`, and a marker that names it is left
out.
`Recording::live` reads a file a writer in the same process is still writing, through the
writer's `Snapshot`, and `Recording::update` takes each newer snapshot, reading only what the
writer added since and decoding only the blocks it hasn't yet, and the open block. Its cost
follows those, not the recording's length. A file cut off by a crash is `Error::Unfinished`; the
format crate's Recutter repairs it.

Index pages, decoded blocks and overview blocks go in a `Cache`, which a recording's snapshots
share. A query holds what it takes until it ends, so it decodes each block once, however many of
its channels, its strip and its statistics read the block, and whatever the cache drops meanwhile.

`Recording::sketch` draws a query from each channel's coarsest level alone: a finished file's
strips, which its footer holds. It reads nothing past the footer, so a host draws it first, while
the exact answer is on the way. It holds no single samples, statistics or markers. A channel
whose list is in pages is sketched from its header alone: without its gaps, breaks and lost
spans, its data running to the length.

A query's work follows what its window holds, not how long the recording is: a channel's runs,
gaps and lost spans are found by binary search, and each run's columns are worked out over the
columns it reaches.

For a reader that wants the samples themselves, as an export does, `Recording::each_sample` gives
each sample's value and its integer code over a range of indices, `index_at` the index at a tick,
`edges` a digital channel's edges, `edge_count` how many there are in a span, from the index with
only the blocks at its ends decoded, and `runs` and `known` where a channel's data lies. Every tick
they take and give is on the recording's time axis. A channel's source keeps its own ticks: `runs`
states an analog channel's samples in them, its methods place them on the axis, and `mapping` gives
the source's `m`, origin and the channel's offset, for a readout exact to the source's tick.

`Recording::query` answers a `Query`: a window in ticks, its columns, the channels drawn, the
statistics and the markers. The columns cover the window and one width either side, so a pan has
columns to draw while its own answer is on the way. A column takes the coarsest overview level no
wider than it among its channel's levels, or the samples and edges themselves below the finest. An
analog trace draws no line across a break. A column that touches a span the instrument lost is
pale: the samples lost at a break, and those lost before a channel's first sample and after its
last. The extent reaches over those, so statistics count them as time no sample measured.

A window that follows a recording as it is written asks for `Query::aligned` columns: each a
whole number of ticks, ⌈width / columns⌉, from tick 0, so every window as wide shares them. The
window's end moves on to the end of its column, and `around` starts on a multiple of a window's
columns, two windows back, so it moves a window at a time. `Recording::extend` answers such a
query from the answer to the one before it: it keeps the columns before the first tick the
snapshots since then changed, works out the rest, and gives exactly what `query` would. A
writer takes each channel's items in that channel's order, so one channel can lag the others:
past a channel's promise its columns are provisional. A channel changes from its promise in the
snapshot before, or from the start of a gap reported since, which can lie further back. It
declines where it can't, and the caller queries whole: the query asks anything else, the window
moved back or by more than its width, `around`'s start moved on, or the answer is another
recording's or older than the 64 snapshots whose changes a recording keeps. Every answer names
the recording and snapshot it was made from.

`Recording::statistics` counts each sample whose time covers part of the interval, one that starts
before it included, and weights its value by the time it covers there, exactly: the measured time
and an integer channel's sum are exact ratios of the axis's ticks, a phase or a stretch included.
Where every sample lasts the same whole number of the recording's ticks, it takes whole overview
buckets where it can. While a recording is
being written, a channel's overview reaches no further than its settled buckets, so the samples
after them are counted one by one.

## Modules

| Module | Ports | Holds |
| --- | --- | --- |
| `readouts` | `web/src/readouts.ts` | intervals, rates, periods, values and counts, with the digits their source carries, in exact arithmetic; `format_code` for an `int` channel's code, exactly |
| `time` | `web/src/time.ts` | a time written as ticks or as a decimal with a unit, to ticks |
| `scale` | `web/src/render/scale.ts` | an analog row's value range: its limits, zoom, move, fit and labels |
| `axis` | `web/src/render/axis.ts` | the time axis's labelled ticks |
| `columns` | `web/src/render/columns.ts` | what each pixel column of a row shows, from edges, samples or overview buckets |
| `probe` | `web/src/probe.ts` | the pulse around a tick, its figures, an analog channel at a pixel, and the searches for a pulse's edges |
| `runs` | `web/src/source.ts` | which sample sits at or around a tick, where a sample's time ends, and a break's lost samples |
| `spans` | `web/src/source.ts` | where a channel's data lies, between its gaps or across its runs |

Each port takes the same inputs and gives the same outputs as its TypeScript module, and both are
tested against the cases in `testdata/view/`.

It builds for `wasm32-unknown-unknown` with its `live` feature off, given clang with the wasm32
target to compile libzstd. `live`, on by default, turns on the format crate's encoder, which holds
the `Snapshot` of a recording a writer in the same process is still writing.

Opening a recording, each query and sketch, each channel drawn, the strip, statistics, every block
decoded, and every page, list, strip, bundle and annotation part read are `tracing` spans at
debug level, so a host's subscriber shows where a view's time goes.

`examples/synth.rs` writes a recording laid out as verge's energy captures are, and
`examples/timing.rs` times opening one and a view's queries of it. With `--trace FILE` it writes
every span as a Chrome trace, and `--view` and `--how` run one case alone for a profiler.
