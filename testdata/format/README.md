# The format's reference vectors

Cases another implementation of FORMAT.md can check itself against. The Rust crates check
themselves against the same files: `crates/format/tests/vectors.rs` reads all four, and
`crates/import/tests/shape.rs` reads the cuts in `places.json`. The invalid recordings aren't
kept here: `crates/format/tests/rules.rs` builds them and writes them on demand (see the last
section).

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
`variants` says. What `recording` leaves out: both channels' `unit` is `"V"` and their `source_name`
their name in lower case; each has one run from tick 0, with no phase and no losses; the one source
is `{"format": "vectors", "files": [], "timebase": [1000, 1], "origin": 0}`; the recording states no
title, meta or `requires`, and `overview_shift` 4. The variants give the derived channel's operation
and operands, the zstd level and block size, and a re-cut at another level and block size. The ids
in `same` are equal and their files differ; the ids in `different` differ. Unlike the other files,
the ids are not worked by hand, since a reader can't work a hash by hand: they pin this
implementation's content ids as of FORMAT.md's v1.

## Invalid recordings

Recordings that each break one rule a reader refuses by, written into an empty directory with

```
TICKWAVE_INVALID_VECTORS=<dir> cargo test -p escapement-tickwave-format --test rules -- --ignored write_the_invalid_vectors
```

`base.tickwave`, `live.tickwave`, `left-out.tickwave` and `plain-live.tickwave` are the valid
recordings the cases are cut from: the first as the encoder writes a whole recording, the second
as the writer writes one while recording, the third as the writer writes one that stops inside a
loss, its last gap record left out of the header, and the fourth a recording of one digital
channel with no gap.

`invalid.json`'s `cases` gives each case's `file`, the FORMAT.md `rule` it breaks, and the
`mode`: the reading that has to refuse it.

- `footer`: a reader with random access before it draws, reading the trailer, the footer, the
  header, the top index, every page and every strip.
- `random`: the same, decoding every block and overview block the index places, and reading the
  annotations through their parts and the bundle.
- `in-order`: a reader in order, from the first record to the trailer.
- `verify`: the full check.

Its `taken` lists files a later minor version may write, cut from `plain-live.tickwave`, which
every reading of this version takes. Each gives its `file`, the `rule` that says so, and the
channels it reads as `unavailable`, by their place among the channels.
