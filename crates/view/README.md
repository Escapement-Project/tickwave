# escapement-tickwave-view

The viewer's logic apart from the drawing, for a native renderer and later the web: readouts with
exact digits, times, the value scale, the time axis, pixel columns and point probes. Each module
takes the same inputs and gives the same outputs as the web viewer's TypeScript module it ports,
and both are tested against the cases in `testdata/view/`.

| Module | Ports | Holds |
| --- | --- | --- |
| `readouts` | `web/src/readouts.ts` | intervals, rates, periods, values and counts, with the digits their source carries, in exact arithmetic; `format_code` for an `int` channel's code, exactly |
| `time` | `web/src/time.ts` | a time written as ticks or as a decimal with a unit, to ticks |
| `scale` | `web/src/render/scale.ts` | an analog row's value range: its limits, zoom, move, fit and labels |
| `axis` | `web/src/render/axis.ts` | the time axis's labelled ticks |
| `columns` | `web/src/render/columns.ts` | what each pixel column of a row shows, from edges, samples or overview buckets |
| `probe` | `web/src/probe.ts` | the pulse around a tick, its figures, and an analog channel at a pixel |
| `runs` | `web/src/source.ts` | which sample sits at or around a tick |

It builds for `wasm32-unknown-unknown` with its `live` feature off. `live`, on by default, turns
on the format crate's encoder, which holds the `Snapshot` of a recording a writer in the same
process is still writing.
