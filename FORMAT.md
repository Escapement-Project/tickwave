# The chrono recording format, version 1.0

A chrono recording holds digital and analog channels of one capture on one time axis, exactly as
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

All integers are little-endian.

| Part | Size | Holds |
| --- | --- | --- |
| preamble | 32 bytes | magic, version, the lengths below |
| header | `header_len` | JSON: what the recording is |
| index | `index_len` | one row a block |
| overview | the rest of the head | a zstd frame: each channel summarised |
| blocks | the rest of the file | one zstd frame a block |

The head is the preamble, header, index and overview: `head_len` bytes from the start. A reader
fetches a fixed amount first (64 KiB), reads `head_len` from the preamble, and fetches the rest of
the head if it needs more.

Every zstd frame carries a content checksum. A reader refuses a frame without one, or with one
that does not match.

### Preamble

| Offset | Type | Field |
| --- | --- | --- |
| 0 | 8 bytes | magic: `89 63 68 72 6f 6e 6f 0a` (`\x89chrono\n`) |
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
| `annotations` | object of strings, optional | a bundled sidecar: each file's name and text |

A channel entry has `name`, `source_name` (the source's own name for it) and `kind`:

- `"digital"`: `initial` (the level at tick 0, a boolean), `end` (the tick up to which the level is
  known; a source can stop knowing one channel before another), `edges` (the count).
- `"analog"`: `unit` (empty when the source states none), `first` (the first sample's tick), `period` (ticks from one sample to the
  next, `[num, den]`), `count`, and `coding`:
  - `{"type": "f32"}`: the source's f32 values.
  - `{"type": "codes", "bits": b, "scale": s, "offset": o}`: u16 codes, stored in two byte
    planes: every code's low byte, then every code's high byte (12 % smaller after zstd than
    interleaved codes on the MSO's analog captures). A sample's value is
    `code × s` in f64, then `o +` that in f64, then rounded to f32: two separately rounded f64
    operations, never a fused multiply-add, which can round to a different f32. In JavaScript,
    `Math.fround(o + code * s)`. A converter picks this coding only after checking that every
    sample reproduces its source value bit for bit.

Sample `i` of an analog channel sits at tick `first + floor(i × period)`.

`source` has `format` (`saleae-binary-1`, `sigrok-sr`, `vcd`), `files` (names, no directories),
and optionally `instrument`, `software`, `begin_s` (the source's own time of tick 0, in seconds, so
a script working from the source can map its times), `extra` (anything else the source states) and
`converter`.

### Index

One row a block, in time order. A row is 24 bytes, then 4 bytes for each channel:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | i64 | the block's start tick |
| 8 | u64 | the block's offset in the file |
| 16 | u32 | its compressed length |
| 20 | u32 | its decoded length |
| 24 + 4c | u32 | channel c: for a digital channel, its edge count in the block, with the level at the block's start in bit 31; for an analog channel, its sample count in the block |

Block i covers ticks `[start_i, start_{i+1})`, and the last block runs to the end. The first block
starts at tick 0. Blocks follow the head with no gaps, and the last one ends at `file_len`.

### Blocks

A block is cut by time across every channel. An encoder takes items in tick order and closes a
block once its decoded size reaches a target. It never closes one between two items at the same
tick, so a sample and an edge at one tick always share a block.

A block's decoded payload holds a u32 section length for each channel, then each channel's section,
in channel order:

- **Digital.** The block's edges, as unsigned LEB128 intervals. The first interval is measured from
  the block's start tick, each later one from the edge before it. Only the first may be zero.
- **Analog.** The block's samples in the channel's coding: f32 values, or u16 codes in two byte
  planes.

### Overview

The overview lets a reader draw a wide view without fetching blocks. Each level summarises every
channel in buckets of `2^shift` ticks; a level has `(length >> shift) + 1` buckets. The levels
the head holds are shifts 12, 16, 20 and so on in steps of 4, keeping those with at most 2^17
buckets and stopping at the first with at most 1,024.

The decoded overview holds, for each level in the header's order, then each channel:

- **Digital.** The level bits, then the change bits, each `ceil(n / 8)` bytes. Bucket i is bit
  `i % 8` of byte `i / 8`. The level bit is the level at the bucket's start; the change bit says
  whether the level changes inside the bucket.
- **Analog.** n minimum values, then n maximum values, as f32. A bucket with no sample holds +∞ and
  −∞. NaN samples are left out.

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
