# The tickwave recording format, version 1.0

A tickwave recording holds digital and analog channels of one capture on one time axis, exactly as
the instrument recorded them. A writer only ever appends to it, so a recorder can stream the file
out while it captures. The file ends with an index. A reader with random access reads the end
first and then only the blocks a view covers, so a page can show a few microseconds of a
two-minute capture without downloading it. A reader that takes the file in order needs no index.
A file cut off at any byte, by a crash, a power cut or a full disk, still reads up to its last whole
record, and so does a file a recorder is still writing.

Status: draft. Version 1 is frozen when the first recording is published; files made before then
may not read.

## Times

Every time is a whole number of ticks from the capture's start, tick 0. One tick is the source's
resolution: 1 ns for a 1 GS/s logic analyser, one sample period for a sigrok capture, the timescale
for VCD, one count of a probe's stamping clock. The header states the tick rate as an exact ratio.
Nothing in the file is finer than a tick, so no reading shows more resolution than the source had.

A converter maps a source time `t` to `round((t − begin) × rate)`, where `begin` is the source's
own start and `rate` its sample rate. It refuses a source whose events sit more than 0.01 of a
tick off that grid.

## Layout

All numbers are little-endian, and an f32 is an IEEE 754 binary32.

| Part | Size | Holds |
| --- | --- | --- |
| preamble | 12 bytes | magic and version |
| records | up to the trailer | the declaration, then blocks, gaps, runs, overview blocks and annotations as they come, then the footer |
| trailer | 24 bytes | where the footer is |

A finished file ends with the footer record and the trailer. A file without them was cut off or is
still being written (see Reading in order, and Repairing a cut-off file).

### Preamble

| Offset | Type | Field |
| --- | --- | --- |
| 0 | 8 bytes | magic: `89 54 4b 57 0d 0a 1a 0a` (`\x89TKW\r\n\x1a\n`) |
| 8 | u16 | major version, 1 |
| 10 | u16 | minor version, 0 |

A minor version adds header fields that older readers ignore, and record kinds from 128 up. A
major version changes the layout; a reader refuses a major version it does not know.

### Records

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u8 | kind |
| 1 | u32 | `n`, the payload's length |
| 5 | `n` bytes | payload |
| 5 + n | 8 bytes | check: the first 8 bytes of the BLAKE3 hash of the kind, `n` and the payload |

| Kind | Record | Holds |
| --- | --- | --- |
| 1 | declaration | what the recording is, before any data |
| 2 | block | every channel's items over one span of ticks |
| 3 | gap | a digital channel's gap |
| 4 | run | where an analog channel's run starts |
| 5 | overview block | part of one overview level |
| 6 | annotation | one generated annotation |
| 7 | footer | the header, the index and the overview |

The declaration is the first record, and only the first. The footer is the last, and the trailer
follows it. The rules below say where the others go.

Kind 0 is never a record, so a tail of zeros, which a power cut can leave, stops a reader. A reader
refuses a record of a kind it does not know from 1 to 127, and skips one from 128 to 255.

### Trailer

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u64 | the footer record's offset in the file |
| 8 | u32 | the footer record's length, its framing included |
| 12 | u16 | major version |
| 14 | u16 | minor version |
| 16 | 8 bytes | magic: `89 54 4b 57 45 4e 44 0a` (`\x89TKWEND\n`) |

The footer record ends where the trailer starts, and a reader refuses a trailer that says
otherwise. The versions repeat the preamble's, for a reader that reads only the end; it refuses a
major version it does not know.

## Declaration

UTF-8 JSON: the header's `timebase`, `channels`, `source`, `title`, `meta` and `overview_shift`,
as far as a writer knows them before any data.

A channel entry has the header's `name`, `source_name` and `kind`. A digital channel has `initial`,
and an analog channel `unit`, `period` and `coding`. What only the end settles is left out: a
digital channel's `end`, `edges` and `gaps`, and an analog channel's `first`, `count` and
`breaks`. A writer that does not know a level at tick 0 declares it false and starts the channel
with a gap from tick 0.

The footer's header holds the same values. It may add keys to `source.extra`, for what the source
states only at its end, such as a probe's totals.

## Header

The footer's header is UTF-8 JSON. Every integer in it is below 2^53, so a JavaScript reader
parses it exactly.

| Field | Type | Meaning |
| --- | --- | --- |
| `timebase` | `[num, den]` | ticks a second |
| `length` | integer | how many ticks the capture covers: ticks `0..length` |
| `channels` | array | one entry a channel, in block order |
| `source` | object | where the recording came from |
| `title` | string, optional | |
| `meta` | object of strings, optional | free keys, such as `probe` or `firmware` |
| `content_id` | string | `blake3:` and the 64 hex digits of the content's digest (below) |
| `blocks` | integer | the number of blocks |
| `overview_shift` | integer | the finest overview level's shift (see Overview) |
| `overview` | array of integers | the overview levels in the footer, as shifts, finest first |
| `overview_blocks` | array, optional | the overview levels in overview blocks, finest first: `{"shift": s, "blocks": n}` each; absent when there are none |
| `annotations` | object of strings, optional | a bundled sidecar: each file's name and text |

A channel entry has `name`, `source_name` (the source's own name for it) and `kind`:

- `"digital"`: `initial` (the level at tick 0, a boolean), `end` (the level is known at ticks
  `0..end`; a source can stop knowing one channel before another), `edges` (the count), and `gaps`
  where the level was not known for a while (see Gaps, below). An `end` of 0 says the level is
  never known. Such a channel has no edges and no gaps, and its `initial` is false.
- `"analog"`: `unit` (empty when the source states none), `first` (the first sample's tick, 0
  when there is no sample), `period` (ticks from one sample to the next, `[num, den]`), `count`,
  `breaks` where the samples stopped for a while (see Gaps), and `coding`:
  - `{"type": "f32"}`: the source's f32 values.
  - `{"type": "codes", "bits": b, "scale": s, "offset": o}`: u16 codes, stored in two byte
    planes: every code's low byte, then every code's high byte. Split this way, the captures we
    tried came out 12 % smaller after zstd than with each code's two bytes together. A sample's
    value is
    `code × s` in f64, then `o +` that in f64, then rounded to f32: two separately rounded f64
    operations, never a fused multiply-add, which can round to a different f32. In JavaScript,
    `Math.fround(o + code * s)`. A converter picks this coding only after checking that every
    sample reproduces its source value bit for bit.

An analog channel's samples come in runs, `period` apart. A channel without `breaks` has one run,
and its sample `i` sits at tick `first + floor(i × period)`.

Every tick the header and the index name lies in `0..length`: every block's start, every first
sample, and every tick a gap or a break names. So does every edge and sample in a block. A digital
channel's `end` lies in `0..=length`. An analog channel has no `end`: its samples may stop before
the length. `length` is at least 1. It is below 2^53, and so are the
terms of `timebase` and `period`, and every `edges`, `count` and break index. No term of
`timebase` or `period` is zero, and `bits` lies in `1..=16`. A reader refuses a header that breaks
any of these before it reads a block.

`source` has `format` (`saleae-binary-0`, `saleae-binary-1`, `sigrok-sr`, `vcd`; a reader keeps
a name it doesn't know), `files` (names, no directories), and optionally `instrument`, `software`,
`begin_s` (the source's own time of tick 0, in seconds, so a script working from the source can
map its times), `trigger_s` (the trigger's time in the same seconds), `rate`, `extra` (anything
else the source states) and `converter`. `rate` is there when the source doesn't record its sample
rate: `inferred` when the converter took the lowest rate on whose grid every edge sits, `given`
when the rate was given to it.

### Gaps

A live recorder can lose data or pause. Its channels then have gaps: spans where nothing is
known. The header says where they are. A sidecar annotation can say why one happened.

A digital channel's `gaps` is an array of `[from, to, level]`. The level is not known at ticks
`from..to`. From `to` on it is `level`, a boolean, until the next edge. No edge lies in
`from..=to`. The gaps go in time order: `0 ≤ from < to < end`, and each gap's `from` lies past
the `to` before it. A channel with a gap from tick 0 has an `initial` of false, since its level at
tick 0 is not known.

An analog channel's `breaks` is an array of `[index, tick]`: sample `index` starts a new run, at
`tick`. The first run starts at `[0, first]`. Sample `i` of the run that starts at `[index, tick]`
sits at `tick + floor((i − index) × period)`. Each `index` lies in `1..count` and is larger than
the one before it. Each `tick` is at least the tick of the sample before it. A reader draws no line
across a break.

A channel with no gap or break leaves the field out.

## Blocks

A block is cut by time across every channel: it holds exactly the edges and samples whose ticks lie
in its span. A writer takes items in tick order and closes a block once its decoded size reaches a
target. It never closes one between two items at the same tick, so a sample and an edge at one tick
always share a block. A writer may also close a block before it reaches the target, at a tick
before which it has every item of every channel. A live recorder does this so a crash loses little,
and such a block can hold no item at all. The format sets no maximum size. A reader may refuse
anything that decodes to more than it is set to hold: a block, an overview block or the footer's
overview. tickwave's reader holds 64 MiB unless it is configured otherwise.

A block record's payload:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | i64 | start tick |
| 8 | i64 | end tick, the next block's start |
| 16 | u32 | decoded length |
| 20 + 4c | u32 | channel c's word, as the index has it |
| 20 + 4C | the rest | the block: one zstd frame |

`C` is the number of channels. The first block starts at tick 0, each later one at the end of the
block before, and every block ends after it starts. The last block ends at the length.

Channel c's word: for a digital channel, its edge count in the block, with the level at the
block's start in bit 31; for an analog channel, its sample count in the block. A digital channel's
level bit is `initial` in block 0. In each later block it is the bit before it, taken through the
earlier block's edges and gap ends in tick order: an edge flips it, and a gap's `to` sets it to the
gap's `level`. Inside a gap, the bit carries the level from before the gap.

Each block, overview block and the footer's overview is one standard zstd frame with a content
checksum, and nothing else: no skippable frame, no second frame, no byte after it, and no
dictionary. A reader refuses a frame without a checksum, with one that does not match, or with
anything else in its bytes. (A decoder that reads every frame it is given would check only the
last checksum, so a valid empty frame after a corrupted block would hide the corruption.)

A block's decoded payload holds a u32 section length for each channel, then each channel's section,
in channel order:

- **Digital.** The block's edges, as unsigned LEB128 intervals. The first interval is measured from
  the block's start tick, each later one from the edge before it. Any of them may be zero: two
  edges at one tick are a pulse the source stamped with a single time, and the level flips twice
  there.
  Every edge lies in `1..end` of its channel (an edge at tick 0 would contradict `initial`), so
  the first block's first interval is never zero.
- **Analog.** The block's samples in the channel's coding: f32 values, or u16 codes in two byte
  planes. Every code is below `2^bits`; a reader refuses a wider one.

## Gap and run records

A gap record:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | i64 | `from` |
| 12 | i64 | `to` |
| 20 | u8 | `level`, 0 or 1 |

A digital channel's gap, as Gaps defines one. It comes before the block that holds tick `to`. The
blocks before that one may already be written when the writer learns of the gap. They stay as they
are, because a block's level bit carries the level from before a gap until the gap's `to`.

A run record:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | u64 | `index` |
| 12 | i64 | `tick` |

Sample `index` of an analog channel starts a run at `tick`. The run at index 0 gives the
channel's `first`, and each later run is a break. It comes before the block that holds sample
`index`.

A gap whose `to` is not below its channel's `end` is left out of the header, and the channel's
`end` becomes the gap's `from`. A gap from tick 0 then leaves an `end` of 0: the level is never
known. A run whose `index` is not below its channel's `count` is left out too.

## Overview

The overview lets a reader draw a view without decoding blocks. Each level summarises every
channel in buckets of `2^shift` ticks; a level has `ceil(length / 2^shift)` buckets.

The finest level's shift is `overview_shift`, at most 62. The levels go from it in steps of 4,
stopping at the first with at most 1,024 buckets or at shift 62. A level with at most 2^17 buckets
goes in the footer. A level with more goes in overview blocks, which a reader fetches like blocks,
for the span it shows. The footer always holds at least one level. The levels follow from `length`
and `overview_shift`, and a reader refuses a header that lists any others.

A writer declares the finest level before it knows the length, so it picks one whose buckets hold
many items. A level as fine as the data makes the overview as large as the blocks, and buckets of a
sparse recording cost room for no data. tickwave's converter takes the first of 12, 16, 20 and so
on with at most 2^26 buckets.

The footer's decoded overview holds, for each level in the header's order, then each channel:

- **Digital.** The level bits, then the change bits, each `ceil(n / 8)` bytes. Bucket i is bit
  `i % 8` of byte `i / 8`. The level bit is the level entering the bucket, before any edge at its
  first tick; the change bit says whether an edge falls in the bucket. The bits follow the block
  words' rule for gaps: inside a gap the level bit carries the level from before it, and a gap's
  `to` sets the change bit of its bucket.
- **Analog.** n minimum values, then n maximum values, as f32. A bucket with no sample holds +∞ and
  −∞. NaN samples are left out.

An overview block record's payload:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u8 | the level's shift |
| 1 | u64 | `k`: the block starts at bucket `65,536 k` |
| 9 | the rest | one zstd frame |

An overview block holds 65,536 buckets of one level, the last block of a level the rest. Its
decoded payload is laid out as one level of the footer's overview, for its own n buckets.

## Annotations

An annotation record's payload is UTF-8 JSON: `{"source": s, "annotation": a}`. `s` names the
generator, as a generator file's first line does, and `a` is one line of that file
([ANNOTATIONS.md](ANNOTATIONS.md)). Its times are ticks.

Annotation records let a live recorder keep what it generates as safely as its data. The footer
does not index them, so a reader with random access never sees them. When a file is finished or
repaired, each source's annotations become its generator file, `rec.<source>.jsonl`, beside the
recording. An annotation that starts at or past the length is dropped, and a span that ends past it
ends at the length.

## Footer

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | `header_len` |
| 4 | u32 | `index_len` |
| 8 | `header_len` bytes | the header |
| 8 + `header_len` | `index_len` bytes | the index |
| 8 + `header_len` + `index_len` | the rest | the overview: one zstd frame |

### Index

One row a block, in time order, then one row an overview block. A block's row is 24 bytes, then 4
bytes for each channel:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | i64 | the block's start tick |
| 8 | u64 | the offset of its zstd frame in the file |
| 16 | u32 | the frame's length |
| 20 | u32 | its decoded length |
| 24 + 4c | u32 | channel c's word, as its block record has it |

Block i covers ticks `[start_i, start_{i+1})`, and the last block runs to `length`. The first block
starts at tick 0, and the starts strictly increase. A reader refuses an index that places an edge
or a sample outside its block.

An overview block's row is 16 bytes: its frame's offset (u64), the frame's length (u32) and its
decoded length (u32). The rows go by level, in the order of `overview_blocks`, then in time order.

## Content id

The content id identifies what a recording holds, not how a file stores it. Re-cutting blocks,
recompressing or bundling annotations leaves it unchanged. A sidecar pins it.

It is BLAKE3 over the timebase (two u64), the length (i64), and then each channel's own BLAKE3
digest, in channel order. A channel's digest covers what the declaration fixes, then the channel's
items in order, then what only the end settles, so a writer computes it as the items arrive.
Strings are written as a u64 byte length and their UTF-8 bytes.

- Digital: the name, `"digital"`, the initial level as one byte, then every edge tick as i64, then
  `end` as i64, the number of gaps as u64, and each gap's `from` and `to` as i64 and its `level` as
  one byte.
- Analog: the name, `"analog"`, the unit, `period` as two u64, then every value's f32 bits as u32,
  then `first` as i64, `count` as u64, the number of breaks as u64, and each break's `index` as u64
  and `tick` as i64.

## Writing

A writer:

- Writes the preamble and the declaration before anything else.
- Takes items in tick order across its channels, and writes each block as it closes.
- Writes a gap or a run before the block that needs it.
- Writes annotation records as it generates them.
- Writes overview blocks as their buckets settle. Overview block k of a level settles once a block
  ends at or past tick `65,536 (k + 1) × 2^shift`. A level goes in overview blocks once a block
  ends past tick `2^(17 + shift)`, since the level then has more than 2^17 buckets: the writer
  writes its settled overview blocks then, and each later one as it settles. Until then it holds
  the level, which may still end up in the footer.
- At the end, writes each overview level's last block, then the footer and the trailer.

A live recorder syncs the file to disk after each block, so how long it keeps a block open bounds
what a crash loses.

## Reading

### With random access

A reader reads the file's last 32 KiB first: over HTTP with `Range: bytes=-32768`, which a server
answers with the whole file when it is shorter. It reads the trailer, and fetches the rest of the
footer if the footer starts before those bytes. Then it fetches the blocks and overview blocks a
view covers, as the index places them.

### In order

A reader takes the records from the start. Each block record carries its span and its words, so
the reader decodes blocks without the footer. It stops at the first record that runs past the end
of the file or whose check does not match, and keeps every record before it. After the footer
record come exactly the trailer's 24 bytes.

A viewer reading a file that is still being written meets a last record cut off where the writer
has got to. It reads that record again once the file grows. The declaration fixes the overview's
levels, so the viewer extends each level bucket by bucket as blocks arrive, and adds a coarser
level once the length needs one.

## Repairing a cut-off file

A cut-off file is repaired by appending a footer and a trailer after its last good record:

1. Take the records up to the first bad one, and cut the file there. Refuse a file with no
   declaration or no block.
2. The length is the last block's end, and every digital channel's `end` is the length, before
   the gap rule above.
3. Count each channel's edges and samples from the blocks' words.
4. Compute the overview and the content id from every block's items, decoding each block once.
   Keep the overview blocks already written, and write the rest.
5. Write the footer and the trailer, and each annotation source's generator file.

A repaired file holds what a writer that had finished at the same block would have written, and so
has the same content id.
