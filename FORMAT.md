# The tickwave recording format, version 1.0

A tickwave recording holds digital and analog channels of one capture on one time axis, exactly as
the instrument recorded them. A reader fetches the head first and then only the blocks a view
covers, so a page can show a few microseconds of a two-minute capture without downloading it.

Status: draft. Version 1 is frozen when the first recording is published; files made before then
may not read.

## Times

Every time is a whole number of ticks from the capture's start, tick 0. One tick is the source's
resolution: 1 ns for a 1 GS/s logic analyser, one sample period for a sigrok capture, the timescale
for VCD. The header states the tick rate as an exact ratio. Nothing in the file is finer than a
tick, so no reading shows more resolution than the source had.

A converter maps a source time `t` to `round((t − begin) × rate)`, where `begin` is the source's
own start and `rate` its sample rate. It refuses a source whose events sit more than 0.01 of a
tick off that grid.

## Layout

All numbers are little-endian, and an f32 is an IEEE 754 binary32.

| Part | Size | Holds |
| --- | --- | --- |
| preamble | 32 bytes | magic, version, the lengths below |
| header | `header_len` | JSON: what the recording is |
| index | `index_len` | one row a block, then one row an overview block |
| overview | the rest of the head | a zstd frame: each channel summarised |
| blocks | after the head | one zstd frame a block |
| overview blocks | the rest of the file | one zstd frame an overview block, for the finer overview levels |

The head is the preamble, header, index and overview: `head_len` bytes from the start. A reader
fetches a fixed amount first (32 KiB), reads `head_len` from the preamble, and fetches the rest of
the head if it needs more.

Each block, overview block and the head's overview is one standard zstd frame with a content
checksum, and nothing else: no skippable frame, no second frame, no byte after it, and no
dictionary. A reader refuses a frame without a checksum, with one that does not match, or with
anything else in its bytes. (A decoder that reads every frame it is given would check only the
last checksum, so a valid empty frame after a corrupted block would hide the corruption.)

### Preamble

| Offset | Type | Field |
| --- | --- | --- |
| 0 | 8 bytes | magic: `89 54 4b 57 0d 0a 1a 0a` (`\x89TKW\r\n\x1a\n`) |
| 8 | u16 | major version, 1 |
| 10 | u16 | minor version, 0 |
| 12 | u32 | `header_len` |
| 16 | u32 | `index_len` |
| 20 | u32 | `head_len` |
| 24 | u64 | `file_len`, the whole file; a shorter file is truncated |

A minor version adds header fields that older readers ignore. A major version changes the layout;
a reader refuses a major version it does not know.

### Header

UTF-8 JSON. Every integer in it is below 2^53, so a JavaScript reader parses it exactly.

| Field | Type | Meaning |
| --- | --- | --- |
| `timebase` | `[num, den]` | ticks a second |
| `length` | integer | ticks from tick 0 to the capture's end |
| `channels` | array | one entry a channel, in block order |
| `source` | object | where the recording came from |
| `title` | string, optional | |
| `meta` | object of strings, optional | free keys, such as `probe` or `firmware` |
| `content_id` | string | `blake3:` and a hex digest of the content (below) |
| `blocks` | integer | the number of blocks |
| `overview` | array of integers | the overview levels in the head, as shifts, finest first |
| `overview_blocks` | array, optional | the overview levels in overview blocks, finest first: `{"shift": s, "blocks": n}` each; absent when there are none |
| `annotations` | object of strings, optional | a bundled sidecar: each file's name and text |

A channel entry has `name`, `source_name` (the source's own name for it) and `kind`:

- `"digital"`: `initial` (the level at tick 0, a boolean), `end` (the tick up to which the level is
  known; a source can stop knowing one channel before another), `edges` (the count).
- `"analog"`: `unit` (empty when the source states none), `first` (the first sample's tick),
  `period` (ticks from one sample to the next, `[num, den]`), `count`, and `coding`:
  - `{"type": "f32"}`: the source's f32 values.
  - `{"type": "codes", "bits": b, "scale": s, "offset": o}`: u16 codes, stored in two byte
    planes: every code's low byte, then every code's high byte. Split this way, the captures we
    tried came out 12 % smaller after zstd than with each code's two bytes together. A sample's
    value is
    `code × s` in f64, then `o +` that in f64, then rounded to f32: two separately rounded f64
    operations, never a fused multiply-add, which can round to a different f32. In JavaScript,
    `Math.fround(o + code * s)`. A converter picks this coding only after checking that every
    sample reproduces its source value bit for bit.

Sample `i` of an analog channel sits at tick `first + floor(i × period)`.

Every tick in the file lies in `0..=length`, and `length` is below 2^53: a digital channel's `end`,
every edge, every sample, every block's start. So do the terms of `timebase` and `period`, and
every `edges` and `count`. No term of `timebase` or `period` is zero, and `bits` lies in `1..=16`.
A reader refuses a header that breaks any of these before it reads a block.

`source` has `format` (`saleae-binary-1`, `sigrok-sr`, `vcd`), `files` (names, no directories),
and optionally `instrument`, `software`, `begin_s` (the source's own time of tick 0, in seconds, so
a script working from the source can map its times), `extra` (anything else the source states) and
`converter`.

### Index

One row a block, in time order, then one row an overview block. A block's row is 24 bytes, then 4
bytes for each channel:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | i64 | the block's start tick |
| 8 | u64 | the block's offset in the file |
| 16 | u32 | its compressed length |
| 20 | u32 | its decoded length |
| 24 + 4c | u32 | channel c: for a digital channel, its edge count in the block, with the level at the block's start in bit 31; for an analog channel, its sample count in the block |

Block i covers ticks `[start_i, start_{i+1})`, and the last block runs to the end. The first block
starts at tick 0, and the starts strictly increase. A digital channel's level bit is `initial` in
block 0, and in each later block the bit before it, flipped once for every edge that block
counted.

An overview block's row is 16 bytes: its offset (u64), compressed length (u32) and decoded length
(u32). The rows go by level, in the order of `overview_blocks`, then in time order.

The blocks follow the head with no gaps, then the overview blocks in the order of their rows. The
last one ends at `file_len`.

### Blocks

A block is cut by time across every channel: it holds exactly the edges and samples whose ticks lie
in its span, and a reader refuses an index that places any elsewhere. An encoder takes items in tick
order and closes a block once its decoded size reaches a target. It never closes one between two items at the same
tick, so a sample and an edge at one tick always share a block. The format sets no maximum size. A
reader may refuse anything that decodes to more than it is set to hold: a block, an overview block
or the head's overview. tickwave's reader holds 64 MiB unless it is configured otherwise.

A block's decoded payload holds a u32 section length for each channel, then each channel's section,
in channel order:

- **Digital.** The block's edges, as unsigned LEB128 intervals. The first interval is measured from
  the block's start tick, each later one from the edge before it. Only the first may be zero.
  Every edge lies in `1..=end` of its channel (an edge at tick 0 would contradict `initial`), so
  the first block's first interval is never zero.
- **Analog.** The block's samples in the channel's coding: f32 values, or u16 codes in two byte
  planes. Every code is below `2^bits`; a reader refuses a wider one.

### Overview

The overview lets a reader draw a view without decoding blocks. Each level summarises every
channel in buckets of `2^shift` ticks; a level has `(length >> shift) + 1` buckets.

The levels are shifts 12, 16, 20 and so on in steps of 4, stopping at the first with at most 1,024
buckets. A level with at most 2^17 buckets goes in the head. A finer level with at most 2^26
buckets goes in overview blocks, which a reader fetches like blocks, for the span it shows. A level
with more is left out. The head always holds at least one level. The levels follow from `length`
alone, and a reader refuses a header that lists any others.

The head's decoded overview holds, for each level in the header's order, then each channel:

- **Digital.** The level bits, then the change bits, each `ceil(n / 8)` bytes. Bucket i is bit
  `i % 8` of byte `i / 8`. The level bit is the level entering the bucket, before any edge at its
  first tick; the change bit says whether an edge falls in the bucket.
- **Analog.** n minimum values, then n maximum values, as f32. A bucket with no sample holds +∞ and
  −∞. NaN samples are left out.

An overview block holds 65,536 buckets of one level, the last block of a level the rest: block k
starts at bucket `65,536 k`. Its decoded payload is laid out as one level of the head's overview,
for its own n buckets, and its row's decoded length is exactly that size.

### Content id

The content id identifies what a recording holds, not how a file stores it. Re-cutting blocks,
recompressing or bundling annotations leaves it unchanged. A sidecar pins it.

It is BLAKE3 over the timebase (two u64), the length (i64), and then each channel's own BLAKE3
digest, in channel order. A channel's digest covers, with strings written as a u64 byte length and
their UTF-8 bytes:

- digital: the name, `"digital"`, the initial level as one byte, `end` as i64, then every edge
  tick as i64;
- analog: the name, `"analog"`, the unit, `first` as i64, `period` as two u64, `count` as u64,
  then every value's f32 bits as u32.
