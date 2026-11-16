# The format's reference vectors

Cases another implementation of FORMAT.md can check itself against. The Rust crates check
themselves against the same files: `crates/format/tests/vectors.rs` reads all four, and
`crates/import/tests/shape.rs` reads the cuts in `places.json`.

| File | What it holds | FORMAT.md |
| --- | --- | --- |
| `places.json` | analog sample places, their axis ticks and exact axis times, the time each sample covers, lost spans, and cuts | Times, Header, Gaps, Moved and trimmed channels |
| `overview.json` | density from edge counts, duty from kept and high ticks, and a crafted digital channel's levels | Overview |
| `parts.json` | the annotation parts a writer cuts two generators' lines into, and the table's top entry | Annotations, Index |
| `content-ids.json` | one recording written five ways, and the content id each has | Content id |

`make-vectors.py` writes `places.json`, `overview.json` and `parts.json` from FORMAT.md's rules,
with exact fractions and big integers and none of tickwave's code. Run it with
`uv run testdata/format/make-vectors.py`.

## places.json

`places` holds channels, each with a `grid` (`period` and `phase`, `[num, den]`), its `first`
tick, its `breaks` as `[index, tick, lost, phase]` with `phase` in `1/D` ticks, its `count` and
its `map` (`origin`, `offset` and `m`, as in `T = origin + floor((k + offset) × m)`). For each
listed sample, `expect` gives:

- `d`: the channel's grid `D`.
- `place`: the sample's place, in `1/D` source ticks.
- `axis`: the axis tick it maps to.
- `axis_time`: its exact axis time, `[num, den]` in lowest terms.
- `time`: the time it covers, `[from, to]` in `1/D` source ticks, before any cut at a kept span
  or the length.

`lost` gives each break with lost samples, by its place in `breaks`, and the span they would have
covered, in `1/D` source ticks.

`trims` holds cuts: a channel and the source tick `from` it is kept from. `expect` gives the cut
channel's `first`, `phase`, `breaks`, `count` and `D`, and each kept sample's place in `1/D`
ticks of the new grid.

## overview.json

`density` gives a bucket's density byte for its edge count, including counts below 2^53 where a
floating-point `log2` lands on the wrong side of a step. `duty` gives the duty nibble for a
bucket's kept ticks `k` and high ticks `h`. `channels` holds digital channels on an axis that is
their source's: `initial`, `edges` and `gaps` (`[from, to, level]`), `length`, the finest
level's `shift`, and `kept` where the channel keeps a span (a second channel kept from 0 goes
beside it). `expect` gives each level's density and duty bytes, finest first, bucket by bucket.

## parts.json

Two generators, each a `count` of lines `{"id":"<first letter>/<i>","start":<first + step × i>,
"text":"<x repeated text times>"}`, written in order of their starts, the first generator first
at one start. `expect` gives the parts a writer that cuts a generator's part once its lines since
the last reach 64 KiB writes, in the order of their rows: each one's generator, its first line's
`i`, its line count and its span. `top` is the table's top index entry: the earliest start and
the latest end.

## content-ids.json

A recording of two `int` channels and a derived one, described in `recording`, written as each of
`variants` says: the derived channel's operation and operands, the zstd level and block size, and
a re-cut at another level and block size. The ids in `same` are equal and their files differ; the
ids in `different` differ. Unlike the other files, the ids are not worked by hand, since a reader
can't work a hash by hand: they pin this implementation's content ids as of FORMAT.md's v1.
