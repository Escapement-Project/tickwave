# escapement-tickwave-view

The viewer's logic apart from the drawing, for a native renderer and later the web. It opens a
recording, finished or still being written, and answers what a view asks of it: each channel's
pixel columns, single samples once there are few enough, the overview strip, statistics over an
interval, and the markers. It also has the web viewer's readouts with exact digits, times, value
scale, time axis and point probes.

## A recording and a query

`Recording::open` reads a finished file through its footer, from any `Bytes`: a file, or bytes in
memory. `Recording::live` reads a file a writer in the same process is still writing, through the
writer's `Snapshot`, and `Recording::update` takes each newer snapshot, reading only what the
writer added since. A file cut off by a crash is `Error::Unfinished`; the format crate's Recutter
repairs it. Decoded blocks and overview blocks go in a `Cache`, which a recording's snapshots share.

A query's work follows what its window holds, not how long the recording is: a channel's runs,
gaps and lost spans are found by binary search, and each run's columns are worked out over the
columns it reaches.

For a reader that wants the samples themselves, as an export does, `Recording::each_sample` gives
each sample's value and its integer code over a range of indices, `index_at` the index at a tick,
`edges` a digital channel's edges, and `runs` and `known` where a channel's data lies.

`Recording::query` answers a `Query`: a window in ticks, its columns, the channels drawn, the
statistics and the markers. The columns cover the window and one width either side, so a pan has
columns to draw while its own answer is on the way. A column takes the coarsest overview level no
wider than it, or the samples and edges themselves below the finest. An analog trace draws no line
across a break, and a column that touches a span the instrument lost is pale.

`Recording::statistics` counts each sample whose time covers part of the interval, one that starts
before it included, and weights its value by the ticks it covers there, exactly for integer codes.
For a period of whole ticks it takes whole overview buckets where it can. While a recording is
being written, its overview reaches no further than the end of the last block the writer closed,
so the samples after that are counted one by one.

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

Opening a recording, each query, each channel drawn, the strip, statistics and every block
decoded are `tracing` spans at debug level, so a host's subscriber shows where a view's time goes.

`examples/synth.rs` writes a recording laid out as verge's energy captures are, and
`examples/timing.rs` times opening one and a view's queries of it. With `--trace FILE` it writes
every span as a Chrome trace, and `--view` and `--how` run one case alone for a profiler.
