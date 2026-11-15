# The tickwave recording format, version 1.0

A tickwave recording holds digital and analog channels on one time axis, exactly as the instruments
recorded them: one capture's, or several captures merged, each channel in the ticks of the
instrument that recorded it. Each channel is a stream of its own on that axis: its blocks, its
overview and its index cost what its own data costs. A writer only ever appends to the file, so a
recorder can stream it out while it captures. The file ends with a small footer. A reader with
random access reads the end first and then only the channels and spans a view shows, so a page can
show a few microseconds of one channel of a long capture without downloading the rest. A reader
that takes the file in order needs no footer. A file cut off at any byte, by a crash, a power cut
or a full disk, still reads up to its last whole record, and so does a file a recorder is still
writing.

Status: draft. Version 1 is frozen when the first recording is published; files made before then
may not read.

## Times

Every time a channel stores is a whole number of ticks of its source. A source's tick is its
resolution: 1 ns for a 1 GS/s logic analyser, one sample period for a sigrok capture, the timescale
for VCD, one count of a probe's stamping clock. Each source states its tick rate as an exact ratio.
A channel holds its source's ticks as the source gave them, so nothing in it is finer than a tick
of its source, and no reading shows more resolution than the source had.

A source's time `t`, in its own seconds, is at source tick `k = round((t − begin_s) × rate)`, where
`begin_s` is the source's own time at its tick 0 and `rate` its tick rate. A converter refuses a
source whose events sit more than 0.01 of a tick off that grid. Every source tick a channel stores
lies in `0..2^53`.

The channels share one time axis, which ticks at the recording's `timebase`. Blocks, the overview,
annotations and the `length` are in the axis's ticks. Channel c of source s holds source tick `k`
at the axis's tick

    T = origin_s + floor((k + offset_c) × m_s),   m_s = stretch_s × R / rate_s

`R` is the recording's `timebase` and `rate_s` the source's. `origin_s` is the source's `origin`, an
axis tick. `offset_c` is the channel's `offset`, in its source's ticks, 0 where it states none.
`stretch_s` is the source's `stretch`, 1 where it states none (see Sources). `m_s` is how many axis
ticks one of the source's ticks lasts. It is an exact ratio, and positive. In lowest terms it is
`a/b`, both below 2^53, and the mapping is integer arithmetic:

    T = origin_s + floor((k + offset_c) × a / b)

`floor` rounds toward −∞: `floor(−7/2)` is −4. The product `(k + offset_c) × a` has a magnitude
below 2^107, so a reader computes it in 128-bit integers. In JavaScript that is BigInt, whose
division rounds toward 0: where the product is negative and leaves a remainder, take 1 off the
quotient, since `-1n / 2n` is `0n` and `floor(−1/2)` is −1. A later source tick never maps to an
earlier axis tick, so a channel's items keep their order on the axis. Where `m_s` is below 1, two
source ticks can share an axis tick.

`m_s` is `(stretch num × R num × rate den) / (stretch den × R den × rate num)`. To find `a` and `b`,
a reader takes each of the nine pairs of a term on top and a term below, and divides both by their
greatest common divisor, before multiplying. The two products are then in lowest terms, and a
reader refuses a source whose `a` or `b` reaches 2^53.

A recording of one source ticks at the source's rate, so its `m` is its stretch, and without one
`T = k + origin + offset`. A reader refuses a recording of one source whose `timebase` has another
value than the source's. A recording of several sources ticks at the least common multiple of
their rates: the slowest rate at which each source's tick is a whole number of axis ticks,
`lcm(p_i) / gcd(q_i)` for rates `p_i/q_i` in lowest terms. Each source without a stretch then has
a whole-number `m`. Where that rate would put a term of the timebase, a source's `m` or the length
at 2^53 or past it, the recording ticks at the fastest of the sources' rates instead, and a slower
source's `m` is a ratio. A stretch multiplies its source's `m`, whatever the rate. A reader maps a
recording of several sources by the `timebase` it states, whichever rate that is.

Every term of the mapping is an integer or an exact ratio in the header, so a script maps a
source's ticks to the axis exactly. A channel stores its source's ticks, so a check against the
source compares them directly. A recording converted whole has the source's timebase, an origin of
0 and no offset or stretch: `T = k`, and it starts where the source does.

## Layout

All numbers are little-endian, and an f32 is an IEEE 754 binary32.

| Part | Size | Holds |
| --- | --- | --- |
| preamble | 12 bytes | magic and version |
| records | up to the trailer | the declaration, then blocks, known-through records, gaps, runs, overview blocks, pages and annotations as they come, then the annotation bundle and the footer |
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
| 2 | block | one channel's items over one span of the axis |
| 3 | gap | a digital channel's gap |
| 4 | run | where an analog channel's run starts |
| 5 | overview block | part of one level of one channel's overview |
| 6 | annotation | one generated annotation |
| 7 | footer | the header, the top index and every channel's strip |
| 8 | known through | a tick up to which every channel is known |
| 10 | index page | one channel's block rows or overview rows, or a directory of pages |
| 11 | list page | one channel's gaps or breaks |
| 12 | annotation bundle | the sidecar's files, bundled at the finish |

The declaration is the first record, and only the first. The footer is the last, and the trailer
follows it. The rules below say where the others go.

Kind 0 is never a record, so a tail of zeros, which a power cut can leave, stops a reader. A reader
refuses a record of a kind it does not know from 1 to 127, kind 9 included, and skips one from 128
to 255.

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

UTF-8 JSON: the header's `timebase`, `channels`, `derived`, `sources`, `title`, `meta` and
`overview_shift`, as far as a writer knows them before any data.

A channel entry has the header's `name`, `source_name`, `source` and `kind`, and `offset`,
`offset_kind`, `kept`, `significant` and `overview_shift` where it states them. A digital channel
has `initial`, and an analog channel `unit`, `period` and `coding`. What only the end settles is
left out: a digital channel's `end`, `edges` and gaps, an analog channel's `first`, `count`, breaks
and losses, and every channel's `blocks` and `overview_blocks`. A writer that does not know a
level at the channel's start (see Header) declares it false and starts the channel with a gap from
its start.

The footer's header holds the same values. It may add keys to a source's `extra`, for what the
source states only at its end, such as a probe's totals.

## Header

The footer's header is UTF-8 JSON. Every integer in it is below 2^53, so a JavaScript reader
parses it exactly.

| Field | Type | Meaning |
| --- | --- | --- |
| `timebase` | `[num, den]` | the axis's ticks a second (see Times) |
| `length` | integer | how many axis ticks the capture covers: ticks `0..length` |
| `channels` | array | one entry a channel, in channel order |
| `derived` | array, optional | channels computed from others, not stored in blocks (see Derived channels) |
| `sources` | array | where the channels came from, one entry a source (see Sources) |
| `title` | string, optional | |
| `meta` | object of strings, optional | free keys, such as `probe` or `firmware` |
| `content_id` | string | `blake3:` and the 64 hex digits of the content's digest (below) |
| `overview_shift` | integer | the finest overview level's shift of a channel that states none (see Overview) |

A channel entry has `name`, `source_name` (the source's own name for it), `source` (its source's
index in `sources`) and `kind`, and `offset`, `offset_kind` and `kept` where it states them (see
Moved and trimmed channels). It can state `overview_shift`, its finest overview level's shift. It
has `blocks`, the number of its blocks, and `overview_blocks` where any of its overview levels go
in overview blocks: one `{"shift": s, "blocks": b, "rows": r}` a level, finest first, with `b` the
level's overview blocks and `r` its index rows (see Overview and Index). An analog channel can
state `significant`, an integer of at least 1: its readings carry at most that many significant
figures, as an instrument whose step changes with its range does.

- `"digital"`: `initial` (the level at the channel's start before any edge there, a boolean), `end`
  (a source tick: the level is known from the channel's start to `end`; a source can stop knowing
  one channel before another), `edges` (the count), and its gaps where the level was not known for
  a while (see Gaps, below). A channel's start is its kept span's `from` (see Moved and trimmed
  channels). In a recording that states no kept span it is the source tick at the axis's tick 0,
  `−origin`. An `end` at the channel's start says the level is never known. Such a channel has no
  edges and no gaps, and its `initial` is its declaration's, which says nothing of the level.
- `"analog"`: `unit` (empty when the source states none), `first` (the first sample's source tick,
  0 when there is no sample), `period` (source ticks from one sample to the next, `[num, den]`),
  `count`, its breaks where the samples stopped for a while and its losses at either end (see
  Gaps), and `coding` (see Codings).

An analog channel's samples come in runs, `period` apart. A channel without breaks has one run, and
its sample `i` sits at source tick `first + floor(i × period)`, its place, and on the axis at the
tick its place maps to.

Every axis tick the header and the index name lies in `0..length`: every block's start. Every source
tick a channel names lies in `0..2^53` and maps into `0..length` (see Times): every edge and every
sample's place, every tick a gap or a break names, the start of a loss before the first sample, and
a kept span's `from`. A digital channel's `end`, a kept span's `to`, and the end of a loss after the
last sample map into `0..=length`. An analog channel has no `end`: its samples may stop before the
length. In a recording that states no kept span, the source's `origin` is at most 0, so each
channel's start maps to tick 0. `length` is at least 1. It is below 2^53, and so are the terms of
every `timebase`, `period` and `stretch`, the terms of each source's `m` in lowest terms, and every
`edges`, `count`, `blocks`, `rows`, break index and lost count. Every `origin`, `align` and `offset`
has a magnitude below 2^53. No term of a `timebase`, a `period` or a `stretch` is zero. A reader
refuses a header that breaks any of these before it reads a block.

### Sources

`sources` lists where the channels came from. A recording converted from one capture has one
entry; a merged one has an entry for each source of each recording merged into it. Every entry is
some channel's source. A merged recording takes each entry from the recording merged into it, with
`origin` and `align` restated on its own axis, `stretch` multiplied by any the merge states for that
recording, and every other field kept. A value `x` on that recording's axis is restated as
`floor(x × s × R_merged / R_input)`, where `s` is the stretch the merge states for it, 1 where it
states none. The origin then moves by that recording's alignment and by where the merged recording
starts, and the align by that recording's alignment. Its channels keep the source ticks they had
there, so a check against that recording compares them directly, and maps the header's values by
the same rule.

An entry has:

- `format`: `saleae-binary-0`, `saleae-binary-1`, `sigrok-sr` or `vcd` for a capture tickwave
  converts, or the name a recorder gives its own stream. A reader keeps a name it doesn't know.
- `files`: names, no directories. A recorder's own stream reads no file, and lists none.
- `timebase`: the source's tick rate, `[num, den]`. It can be any exact ratio: a recorder that
  measured its clock against a reference states the rate it measured. One of its ticks lasts `m`
  axis ticks (see Times).
- `origin`: an integer, the axis tick where the source's tick 0 lies before any channel's `offset`
  (see Times). Every move, cut and alignment of the source's channels folds into it.
- Optionally `align`: axis ticks that whoever merged it moved this source by against the others,
  positive later, to line up instruments that saw the same event. Like `offset`, it is a claim, not
  something an instrument recorded. Instruments' clocks drift: at 20 ppm apart, an alignment by hand
  stays within one tick of a 96 MHz clock for about ±0.5 ms around the point where it was made, and
  within 10 µs for about ±0.5 s.
- Optionally `stretch`: `[num, den]`, how much whoever merged it lengthened this source's ticks
  against its `timebase`, to line up an instrument whose clock ran at another rate than it states.
  The source's tick 0 stays at `origin`, and on the axis its tick `k` lies `k × stretch / rate`
  seconds after it, before the mapping's floor. A stretch above 1 says the source's clock ran slow.
  Like `align`, it is a claim. Two events both instruments saw give both: `align` from one, and
  `stretch` from the time between them. What a stretch leaves is the clocks' wander. 1 where it
  states none.
- Optionally `title` and `meta`: the title and metadata of the recording a merge took the source
  from, since the merged recording's own `title` and `meta` are the merge's. A source that came
  from a merge already keeps those of its own recording.
- Optionally `instrument`, `software`, `begin_s` (the source's own time at its tick 0, in seconds),
  `trigger_s` (the trigger's time in the same seconds), `rate`, `extra` (anything else the source
  states) and `converter`. `rate` is there when the source doesn't record its sample rate:
  `inferred` when the converter took the lowest rate on whose grid every edge sits, `given` when
  the rate was given to it.

A reading of one channel keeps to its source's tick, and a reading between two sources' channels,
an alignment's included, to the coarser of the two. A reading tied to no channel keeps to the finest
source's tick. On the axis a source's tick lasts `m` ticks, not always a whole number. A reading of
times within one channel takes them from its source ticks, which the file holds exactly, rather
than from the axis ticks they map to.

### Codings

An analog channel's `coding` says how its samples are stored:

- `{"type": "f32"}` and `{"type": "f64"}`: the source's floats.
- `{"type": "int", "bits": b, "signed": s}`, with `b` in `1..=64` and `s` a boolean: integer
  samples, each a value exactly. A signed sample lies in `−2^(b−1)..2^(b−1)`, an unsigned one in
  `0..2^b`, and a reader refuses one outside. It can also state `"scale": [num, den]` and
  `"offset": [num, den]`, each an exact ratio with a positive denominator, a nonzero `scale` and
  terms whose magnitude is below 2^53. A sample's value is then `offset + code × scale` exactly:
  an ADC's counts, stored as the instrument gave them, with the factor that makes them volts or
  amps. One code step, `scale`, is the finest a reading of it shows. A reader that needs the value
  as an f64 takes the code as an f64, multiplies by the scale's numerator, divides by its
  denominator, and adds the offset's numerator divided by its denominator: each step in f64,
  rounded once, so every reader gets the same float. Without an offset, and with `code × num` below
  2^53, that float is the one nearest the value. In JavaScript, `(code * num) / den + onum / oden`.
  A coding without an `offset` adds nothing.
- `{"type": "codes", "bits": b, "scale": s, "offset": o}`, with `b` in `1..=16`: a smaller store for
  an f32 source. A sample's value is `code × s` in f64, then `o +` that in f64, then rounded to f32:
  two separately rounded f64 operations, never a fused multiply-add, which can round to a different
  f32. In JavaScript, `Math.fround(o + code * s)`. A converter picks this coding only after
  checking that every sample reproduces its source value bit for bit.

A reading of an f32 or f64 channel shows four significant figures unless the channel states
`significant`. A reading of an `int` channel keeps to one code step and to `significant` where
stated, whichever is coarser.

### Derived channels

A derived channel is computed from two analog channels, so a reader draws it without the recording
storing its samples. `derived` holds one entry each: `{"name": n, "unit": u, "product": [a, b]}`,
where `a` and `b` name analog channels, as power is current times voltage. No two channels or
derived channels share a name, and a reader refuses a header where any do. Its operands come from
one source, and share `first`, `period`, `count`, `breaks`, their losses at either end and their
move, the channel's `offset` (0 where a channel states none), so its sample i sits where theirs
does:

- When both are `int` channels whose codings state no `offset`, their `bits` sum to at most 64,
  and their scales' numerators multiply to below 2^53 and so do their denominators (a scale a
  channel doesn't state is 1), sample i is the integer product of their codes, signed if either is,
  with `scale` the product of their scales, exactly. Its value as an f64 follows the rule for an
  `int` channel.
- Otherwise it is `f64(a_i) × f64(b_i)`, rounded once, where an `int` operand's f64 follows that
  rule.

A derived entry can state `significant`, as an analog channel can. A reading of an integer
product follows the rule for an `int` channel, and one of an f64 product the rule for a float.

Blocks hold nothing for a derived channel. It has an overview of its own, so a reader draws it
zoomed out and measures it without decoding blocks, and its entry has `overview_blocks` as a
channel's does. Derived channels are numbered after the channels: derived channel `d` is overview
channel `C + d`, where `C` is the number of channels.

### Moved and trimmed channels

Whoever converts a capture can move a channel along the time axis by a whole number of its source's
ticks, to correct a known delay in its path, and keep only part of a channel. Neither is what the
instrument recorded, so the recording states them: a moved channel its `offset`, and, once any
channel is moved or cut, every channel the span that holds its source data, `kept`. A recording
with neither is as the source recorded it.

- `offset`: the source ticks the channel was moved by, an integer; positive is later (see Times for
  how it maps). A converter refuses an offset more than 0.01 of the source's tick off its grid.
- `offset_kind`: why it was moved, stated with every `offset`. `"delay"` is a known delay in the
  channel's path, such as a probe's lead or an instrument's input. A reader keeps a kind it doesn't
  know, and refuses an `offset` without one, or one without an `offset`.
- `kept`: `[from, to]`, the source ticks `from..to` where the channel holds what the source
  recorded; `to` is not included. They are ticks as the channel stores them, before its move. On
  the axis the span runs from the tick `from` maps to up to the tick `to` maps to. Outside it the
  channel holds nothing from the source: it was cut, or the source had not started or had stopped.
  Neither is a loss by the instrument. Every channel states `kept` when any channel is moved or cut,
  when the recording has several sources, or when its source states a stretch other than 1, and
  none does otherwise. A reader refuses a recording where only some channels state it, or where all
  do and none starts at the axis's tick 0: no channel's `from` maps to 0. A digital channel starts
  at its `from`: its edges and gaps lie in `from..end`, and its `end` is at most `to`. A cut inside
  a gap the instrument left starts the channel with a gap from `from`, and the instrument can stop
  before the cut does. A reader shows the axis outside the kept span as outside the source's data,
  and a gap as lost. A digital channel known nowhere in its kept span has its `end` at `from` and no
  gap, as any channel never known has. An analog channel's samples lie inside: its `first` is at
  least `from`, and its last sample's place is below `to`. A reader refuses a `kept` that its
  channel's data contradicts.

A cut keeps every edge in its span, one at `from` included, and the channel's `initial` is its level
just before `from`. An analog channel with a period of `p/q` in lowest terms keeps its samples'
places only if its first kept sample is a multiple of `q` samples into the run that holds it,
counted from the index that starts the run. A converter moves a cut forward to the next such
sample, at most `q − 1` samples on.

When no channel is moved or cut, and there is one source with no stretch, the recording starts at
the source's tick `−origin`: where the source does, for a capture converted whole. Otherwise it
starts at the earliest of the channels' starts on the axis, each after its move and its source's
alignment and stretch: a channel starts at the tick its kept span's `from` maps to.

### Gaps

A live recorder can lose data or pause. Its channels then have gaps: spans where nothing is
known. The header says where they are. A sidecar annotation can say why one happened.

A digital channel's gaps are a list of `[from, to, level]`, in its source's ticks. The level is not
known at source ticks `from..to`. From `to` on it is `level`, a boolean, until the next edge. No
edge lies in `from..=to`. The gaps go in time order: `start ≤ from < to < end`, where `start` is
the channel's start (see Header), and each gap's `from` lies past the `to` before it. A channel
with a gap from its start has an `initial` of false, since its level there is not known.

An analog channel's breaks are a list of `[index, tick, lost]`: sample `index` starts a new run, at
source tick `tick`, and the instrument lost `lost` samples before it, 0 when it only paused. The
first run starts at `[0, first]`. Sample `i` of the run that starts at `[index, tick]` sits at
source tick `tick + floor((i − index) × period)`, its place on the run's grid. The grid goes on past
the run's last sample, and before the first run's first sample. Each `index` lies in `1..count` and
is larger than the one before it. Each `tick` is at least the place of the sample before it. A
reader draws no line across a break. The lost samples sit where the run before the break would
have put them: their time runs from place `index` on that run's grid to place `index + lost`, and
ends at `tick` if that comes sooner. The rest of the break is a pause. A reader shows the two apart.

An analog channel can also state samples the instrument lost before its first sample and after its
last, each at least 1, and absent when none:

- `lost_before`: they sit at places `−lost_before` to −1 on the first run's grid, so their time runs
  from the place of `−lost_before` to `first`. A reader refuses a loss whose start lies below source
  tick 0 or maps before axis tick 0.
- `lost_after`: they sit at places `count` to `count + lost_after − 1` on the last run's grid, so
  their time runs from the place of `count` to the place of `count + lost_after`. A reader refuses a
  loss whose end maps past the length, and a writer refuses a length that doesn't cover it.

A reader pales those spans as it pales lost samples inside, and statistics count them as not
measured.

Each sample covers the source ticks from its place to the next place on its run's grid, so inside
a run it reaches the next sample's place. A run's last sample ends at the next run's tick if that
comes sooner. So every sample's time, and every lost span, is a whole number of source ticks. On
the axis a sample covers the ticks from the one its place maps to up to the one its end maps to,
and stops at the length. A reader that measures a span of the axis weighs each sample by the axis
ticks of its time inside the span, and counts a sample whose time reaches into the span even when
its place lies before it.

A list of at most 64 entries is in the header: a digital channel's `gaps` and an analog channel's
`breaks`, each an array of the entries above. A longer list is in list pages (see Index), and the
header states its length instead, as `gap_count` or `break_count`. A channel with no gap or break
states neither.

## Blocks

A channel's items are cut by time into blocks of its own: a block holds exactly the channel's edges
or samples whose axis ticks lie in its span. A channel's blocks tile the axis in order: the first
starts at tick 0, each later one at the end of the one before, every one ends after it starts, and
the last ends at the length. Each channel's blocks are cut apart from every other channel's, so a
slow channel's block can span hours while a fast one's spans milliseconds.

A writer closes a channel's block once its decoded size reaches a target. It never closes one
between two of the channel's items at the same axis tick. A writer may also close a block before
it reaches the target, at an axis tick before which it has every item of the channel. A live
recorder does this so a crash loses little, and such a block can hold no item at all. The format
sets no maximum size. A reader may refuse anything that decodes to more than it is set to hold: a
block, an overview block, a strip or the annotation bundle. tickwave's reader holds 64 MiB unless
it is configured otherwise.

A block record's payload:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | i64 | start, an axis tick |
| 12 | i64 | end, the channel's next block's start |
| 20 | u32 | the channel's word, as the index has it |
| 24 | u32 | decoded length |
| 28 | the rest | the block: one zstd frame |

The word: for a digital channel, its edge count in the block, with the level at the block's start
in bit 31; for an analog channel, its sample count in the block. A digital channel's level bit is
`initial` in its block 0. In each later block it is the bit before it, taken through the earlier
block's edges and gap ends in the order of their source ticks: an edge flips it, and a gap's `to`
sets it to the gap's `level`. A gap ends in the channel's block that holds the axis tick its `to`
maps to. Inside a gap, the bit carries the level from before the gap, and before the channel's
start it carries `initial`.

Each block, overview block, strip and annotation bundle is one standard zstd frame with a content
checksum, and nothing else: no skippable frame, no second frame, no byte after it, and no
dictionary. A reader refuses a frame without a checksum, with one that does not match, or with
anything else in its bytes. (A decoder that reads every frame it is given would check only the
last checksum, so a valid empty frame after a corrupted block would hide the corruption.)

A block's decoded payload is its channel's section:

- **Digital.** Nothing when the block holds no edge. Otherwise unsigned LEB128 numbers: `step`, at
  least 1; the first edge's source tick; then each later edge's interval, its source ticks from the
  edge before it, divided by `step`. A writer takes `step` as the greatest common divisor of the
  block's intervals that are not zero, and 1 when there is none. A source declared at a multiple of
  its rate stamps only multiples of some tick count, so `step` takes that factor out, and its
  blocks come out about the size they have at its own rate. An interval may be zero: two edges at
  one tick are a pulse the source stamped with a single time, and the level flips twice there.
  Every edge lies in `start..end` of its channel, and the axis tick it maps to lies in the block's
  span. An edge at the channel's start flips the level from `initial`, as any edge does, so a pulse
  at the capture's first instant keeps both its edges. A reader refuses a `step` of 0, an edge that
  lies outside those bounds, and bytes left after the last edge.
- **Analog.** The block's samples in the channel's coding. f32 and f64 values are stored whole.
  Codes, of an `int` or a `codes` channel, are stored in byte planes: every code's lowest byte, then
  every code's next byte, and so on for `ceil(bits / 8)` bytes, a signed code in two's complement.
  Split this way, the captures we tried came out 12 % smaller after zstd than with each code's
  bytes together. A reader refuses a code outside its coding's range.

## Known-through records

A known-through record's payload is one i64, `tick`: every item of every channel before axis tick
`tick` is in a block record before this record. So a channel whose last block ends before `tick`
holds no item from that end to `tick`. A gap is not an item: a gap record after a known-through
record may start before its tick, as a recorder that reports a loss when it ends writes one. Each
known-through record's tick is later than the one before it, and no later than the length. A reader
refuses a block record after it that holds an item before its tick.

A live recorder writes one at each flush, after that flush's block records (see Writing), so a
channel with nothing new needs no record of its own, and repair cuts every channel to the same tick.
A flush at the tick of the flush before it writes none.

## Gap and run records

A gap record:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | i64 | `from`, a source tick |
| 12 | i64 | `to`, a source tick |
| 20 | u8 | `level`, 0 or 1 |

A digital channel's gap, as Gaps defines one. It comes before the channel's block that holds the
axis tick `to` maps to. The channel's blocks before that one may already be written when the writer
learns of the gap. They stay as they are, because a block's level bit carries the level from before
a gap until the gap's `to`.

A run record:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | u64 | `index` |
| 12 | i64 | `tick`, a source tick |
| 20 | u64 | `lost` |

Sample `index` of an analog channel starts a run at `tick`, after `lost` samples the instrument
lost. The run at index 0 gives the channel's `first`, and its `lost` is the channel's
`lost_before` (see Gaps); each later run is a break. It comes before the channel's block that holds
sample `index`.

A gap whose `to` is not below its channel's `end` is left out of the header, and the channel's
`end` becomes the gap's `from` if that comes sooner. A gap from the channel's start then leaves its
`end` at its start: the level is never known. A run whose `index` is not below its channel's
`count` is left out too.

## Overview

The overview lets a reader draw a view without decoding blocks. Each channel and each derived
channel has an overview of its own: levels of buckets of `2^shift` axis ticks, each summarising
that channel alone. A level has `ceil(length / 2^shift)` buckets. An item counts in the bucket that
holds the axis tick it maps to.

A channel's finest level's shift is its `overview_shift`, or the recording's where it states none,
at most 62. A derived channel's is the coarser of its operands'. A channel's levels go from it in
steps of 4, stopping at the first with at most 1,024 buckets or at shift 62. That last level is the
channel's strip, in the footer (see Footer). Every other level goes in overview blocks, which a
reader fetches like blocks, for the span it shows. The levels follow from `length` and the shift,
and a reader refuses a header that lists any others in `overview_blocks`.

A writer declares each channel's finest level before it knows the length. tickwave's writers pick
the shift that puts about 64 of the channel's items in a bucket: the whole number nearest
`log2(64 × t)`, held to `0..=62`, where `t` is the channel's axis ticks an item. That is the `s` for
which `(64 × t)²` lies between `2^(2s − 1)` and `2^(2s + 1)`; no ratio falls on either bound. An
analog channel's `t` is its `period` times its source's `m`. A digital channel's is the axis ticks
from its start to its `end`, over its edge count. A channel with no item takes 62. A recorder that
has no data yet takes the interval it expects between the channel's items, such as its sample
period, and a channel whose rate it can't know ahead, such as a digital pin, takes the recording's
shift. Since `t` is in axis ticks, the same data at a finer timebase gets a larger shift and about
as many buckets. A view whose column is narrower than a channel's finest bucket reads the channel's
items, about 64 a bucket.

A channel's level decodes to, for its `n` buckets:

- **Digital.** The level bits, then the change bits, each `ceil(n / 8)` bytes. Bucket i is bit
  `i % 8` of byte `i / 8`. The level bit is the level entering the bucket, before any edge at its
  first tick; the change bit says whether an edge falls in the bucket. The bits follow the block
  words' rule for gaps: inside a gap the level bit carries the level from before it, and a gap's
  `to` sets the change bit of its bucket. Before the channel's start the level bit carries
  `initial`.
- **Analog.** n minimum values, then n maximum values, then n sums of its samples: an `int`
  channel's codes, and every other channel's values. A sample counts in the bucket that holds the
  axis tick its place maps to.

  | Coding | Minimum and maximum | Sum |
  | --- | --- | --- |
  | `f32` | f32 | f64 |
  | `codes` | f32, of the values | f64, of the values |
  | `f64` | f64 | f64 |
  | `int`, unsigned | unsigned, `w` bytes | i128 |
  | `int`, signed | two's complement, `w` bytes | i128 |

  `w` is the smallest of 1, 2, 4 and 8 bytes that holds the coding's `bits`.

  A bucket with no sample holds a minimum above its maximum: +∞ and −∞, or the largest and the
  smallest number its `w` bytes hold. NaN samples are left out. Where −0 and +0 tie for a minimum or
  a maximum, the first in index order counts. A sum starts at +0, so an empty bucket's is 0. A
  bucket of the finest level sums its samples in index order, and a bucket of a coarser level sums
  the 16 buckets of the level below it that it covers, in order, so every writer gives the same
  bytes. An integer sum is exact. A bucket's sample count follows from `first`, `period`, `breaks`
  and the mapping, so the overview doesn't store it.
- **Derived.** As an analog channel: an integer product as signed or unsigned `int` codes whose
  `bits` are its operands' summed, and an f64 product as `f64`.

A level in overview blocks is cut into blocks of 4,096 buckets, the last block the rest. An overview
block record's payload:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel, a derived channel numbered after the channels |
| 4 | u8 | the level's shift |
| 5 | u64 | `k`: its first block starts at bucket `4,096 k` |
| 13 | u32 | `n`, at least 1 |
| 17 | the rest | one zstd frame |

The frame decodes to one block's buckets, laid out as the channel's level for those buckets, and
blocks `k..k + n` of the level are each those bytes. So a run of alike blocks, from a span with
nothing in it or a signal that repeats, costs one record and one row. A level's last block, which
may hold fewer buckets, is always a record of its own with `n` = 1, so a run's blocks are all
whole. tickwave's writer writes each run of alike blocks as one record, up to `n` = 2^32 − 1.

## Annotations

An annotation record's payload is UTF-8 JSON: `{"source": s, "annotation": a}`. `s` names the
generator, as a generator file's first line does, and `a` is one line of that file
([ANNOTATIONS.md](ANNOTATIONS.md)). Its times are axis ticks. A reader refuses a record whose `s`
is not a name ANNOTATIONS.md allows a generator, since the source's file is named after it.
tickwave's writer refuses, at the call, an annotation its generator's file could not hold: one
whose fields, ticks or channels break ANNOTATIONS.md's rules for a line, or whose id its
generator has given already. Its length aside, which the writer learns only when it finishes, each
annotation it takes resolves. Repair and recut leave out a record whose line breaks those rules,
as another writer may have written it, and name each one they leave out. Annotations keep no
order with the channels' items.

Annotation records let a live recorder keep what it generates as safely as its data. The footer
does not index them, so a reader with random access never sees them. When a file is finished or
repaired, each source's annotations become its generator file, `rec.<source>.jsonl`, beside the
recording. A writer can instead bundle them into the file when it finishes, with a hand-written
file that holds no annotation of its own and includes them, so a live recording stays one file. An
annotation that starts at or past the length is dropped, and a span that ends past it ends at the
length.

An annotation bundle record's payload is a u32, the decoded length, then one zstd frame of UTF-8
JSON: an object of strings, each file's name and text. The footer's top index places it. A file
holds at most one, written just before the footer.

## Index

Each channel's blocks, each level of a channel's overview in overview blocks, and each gap or break
list longer than 64 entries is a table: its entries in order. A table's entries go in pages of
4,096, the last page the rest, and the footer's top index lists the pages. Past 16 pages, directory
pages list the pages, 4,096 entries a directory page, and the top index lists the directory pages;
past 16 of those, another directory level goes above, and so on. So the top index holds at most 16
entries a table however long the recording, and a reader holds one page a level at a time.

| Table | Its entries | Their count |
| --- | --- | --- |
| a channel's blocks | block rows | the channel's `blocks` |
| one level of a channel's overview in overview blocks | overview rows | the level's `rows` |
| a digital channel's gaps, past 64 | gaps | `gap_count` |
| an analog channel's breaks, past 64 | breaks | `break_count` |

A block row, 28 bytes:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | i64 | the block's start tick |
| 8 | u64 | the offset of its zstd frame in the file |
| 16 | u32 | the frame's length |
| 20 | u32 | its decoded length |
| 24 | u32 | the channel's word, as its block record has it |

Block i of a channel covers ticks `[start_i, start_{i+1})`, and its last block runs to `length`. The
first starts at tick 0, and the starts strictly increase. A reader refuses an index that places an
edge or a sample outside its block.

An overview row, 20 bytes: its frame's offset (u64), the frame's length (u32), its decoded length
(u32) and `n` (u32), as its overview block record has them. A level's rows go in order of `k`, each
starting where the one before ends, and their `n` sum to the level's `blocks`. A reader refuses a
zero `n`, rows whose `n` sum to another count, and a last row of more than one block.

A gap is 17 bytes, `from` (i64), `to` (i64) and `level` (u8); a break is 24 bytes, `index` (u64),
`tick` (i64) and `lost` (u64), as Gaps defines them.

A page record's payload, for an index page (kind 10) and a list page (kind 11) alike:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel, a derived channel numbered after the channels |
| 4 | u8 | table: 0 blocks, 1 an overview level, 2 gaps, 3 breaks |
| 5 | u8 | the overview level's shift, 0 for any other table |
| 6 | u8 | depth: 0 for a page of the table's entries, `d` for a directory of pages of depth `d − 1` |
| 7 | u64 | its first entry's place among the entries at its depth, counted from 0 |
| 15 | u32 | the number of its entries, 1 to 4,096 |
| 19 | the rest | its entries |

A list page holds gaps or breaks at depth 0; an index page holds block or overview rows at depth 0,
or directory entries at any depth from 1. A directory entry, 28 bytes:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | i64 | `first`, of the first entry under it |
| 8 | u64 | `number`, of the first entry under it |
| 16 | u64 | the page record's offset in the file |
| 24 | u32 | the page record's length, its framing included |

`first` and `number` say where the page it points at starts: for blocks, the first block's start
tick and its first item's index in the channel; for an overview level, the first row's `k` and its
place among the rows; for gaps, the first gap's `from` and its place in the list; for breaks, the
first break's `tick` and its `index`. The top index's entries are directory entries too.

A table's shape follows from its count `R`. Its depth-0 pages are `ceil(R / 4,096)`. Where the pages
at a depth number at most 16, the top index lists them, and that depth is the table's. Otherwise
directory pages one depth up list them, 4,096 entries a page in order, the last the rest. Counts
are below 2^53, so a table has at most four directory levels: pages alone list 2^16 entries, one
directory level 2^28, two 2^40, three 2^52, and a fourth every count past that. A reader refuses a page whose channel, table, shift or depth differ
from the entry that points at it, whose first place or count differ from what the shape gives, or
whose entries are out of order; a directory entry whose `first` or `number` differ from its page's;
and a top index whose depth or entry count for a table differ from what the table's count gives.

A writer writes a table's page once it holds 4,096 entries, after the records its entries place,
and a directory page once it holds 4,096 entries likewise. It writes every table's last pages at
the finish.

## Footer

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | `header_len` |
| 4 | u32 | `index_len` |
| 8 | `header_len` bytes | the header |
| 8 + `header_len` | `index_len` bytes | the top index |
| 8 + `header_len` + `index_len` | the rest | the strips: one zstd frame a channel |

The top index:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u64 | the annotation bundle record's offset in the file, 0 without one |
| 8 | u32 | its length, its framing included, 0 without one |
| 12 | the rest | an entry for each channel, in channel order, then each derived channel |

A channel's entry:

| Type | Field |
| --- | --- |
| u32 | its strip frame's length |
| u32 | the strip's decoded length |
| each of its tables in turn | u8 depth, u8 `e` (1 to 16), then `e` directory entries |

A channel's tables go in this order: its blocks; each level of its overview in overview blocks,
finest first; and its gaps or breaks where the header states `gap_count` or `break_count`. A
derived channel has only its overview levels. A reader refuses a top index that is not exactly
`index_len` bytes.

The strips follow the top index, back to back in the order of its entries. A channel's strip is
its coarsest overview level, all its buckets, laid out as the channel's level for them (see
Overview). A reader refuses a strip whose decoded length is not that level's size, or whose frames
don't end where the footer does. It decompresses only the strips of the channels it shows.

So the footer holds nothing that grows with the recording's length but the number of overview
levels, which grows by one each time the length grows sixteenfold: the header without its long
lists, at most 16 top-index entries a table, and a strip of at most 1,024 buckets a channel. A first
draw reads the trailer and the footer and decompresses the strips it shows.

## Content id

The content id identifies what a recording holds, not how a file stores it. Re-cutting blocks,
recompressing, re-indexing or bundling annotations leaves it unchanged. A sidecar pins it.

It is BLAKE3 over the timebase (two u64), the length (i64), and then each channel's own BLAKE3
digest, in channel order. A channel's digest covers what the declaration fixes, then the channel's
items in order, then what only the end settles, so a writer computes it as the items arrive. Every
tick in it is a source tick, as the channel stores it. Strings are written as a u64 byte length and
their UTF-8 bytes.

Between what the declaration fixes and the items, a channel adds a byte of flags where any is set:
1 if it states `offset`, 2 if it states `kept`, 4 if the recording has several sources or the
channel's source has an `m` other than 1 or an `origin` other than 0. Then `offset` as i64 and
`offset_kind` as a string, where it states them; `kept`'s `from` and `to` as i64, where it states
it; and with flag 4, its source's `m` as two u64 in lowest terms, `origin` as i64, `align` as i64,
0 where the source states none, and `stretch` as two u64 in lowest terms, 1 and 1 where the source
states none. A channel with no flag adds nothing, so a recording converted whole has the id its
data alone gives.

- Digital: the name, `"digital"`, the initial level as one byte, then every edge's source tick as
  i64, then `end` as i64, the number of gaps as u64, and each gap's `from` and `to` as i64 and its
  `level` as one byte.
- Analog: the name, `"analog"`, the unit, `period` as two u64, then the samples, then `first` as
  i64, `count` as u64, the number of breaks as u64, and each break's `index` as u64, `tick` as i64
  and `lost` as u64. Where the channel states `lost_before` or `lost_after`, both follow as u64, 0
  for one it doesn't state. The samples of an `f32` or a `codes` channel are every value's f32 bits
  as u32, so the two codings of one capture's values give one id. An `f64` channel adds `"f64"`
  before the flags and hashes every value's bits as u64. An `int` channel adds, in the same place,
  `"int"`, `bits` as one byte, `signed` as one byte, and `scale` and `offset` as an i64 and a u64
  each (1/1 and 0/1 where it states none), and hashes every code as i64 or u64.

Derived channels, `significant`, `overview_shift`, and a source's fields other than the four flag 4
adds are not part of the content: they describe it, or follow from it. A source's `timebase`
follows from its `m`, its `stretch` and the recording's `timebase`.

## Writing

A writer:

- Writes the preamble and the declaration before anything else.
- Takes each channel's items in the order of its source ticks. The channels keep no order among
  themselves.
- Puts each item in its channel's open block, which starts where the channel's last block ended,
  at tick 0 for its first. No item of a channel lies before the start of its open block.
- Closes a channel's block at an axis tick before which it has every item of the channel, and
  writes it.
- Writes a gap or a run before the channel's block that needs it.
- Writes annotation records as it generates them.
- Writes a known-through record at a tick before which every item of every channel is in a block
  record before it.
- Writes overview blocks as their buckets settle. Block k of a channel's level settles once the
  channel is known past axis tick `4,096 (k + 1) × 2^shift`: one of its blocks ends there or past
  it, or a known-through record's tick does. A run of alike blocks is written when it ends: when a
  block with other bytes settles, or at the finish.
- Writes each table's pages as they fill (see Index).
- At the end, closes every channel's last block at the length, then writes a known-through record
  at the length if it has written any (so a file cut off inside its end repairs to itself), each
  overview level's last block and its open run, every table's last pages and the annotation bundle
  if any, then the footer and the trailer.

A writer refuses, as an error, any tick, count or length that a reader would refuse, the axis tick
an item maps to included, rather than write a file no reader opens.

### tickwave's writer

**Order and promise.** It takes each channel's items in that channel's order, and refuses an item
before the start of its channel's open block. A promise, `promise(before)`, says every item of
every channel before axis tick `before` is in; `promise_channel(c, before)` says it of channel `c`
alone. It refuses an item before the latest promise to its channel. A channel's open block closes
once its items before the latest promise to it and to its partners reach the target size: those go
in the closed block, and those at or after that tick stay in the open one. A derived channel's
operands are partners: they close their blocks together, at one tick. So a producer can write a channel ahead of what it has promised,
as a probe's samples come ahead of the pin edges that lie among them. Without a promise, a block
closes only at a flush or the finish. Promises and flushes go forward: each is at or after the one
before, so every block a writer closes before a flush ends at or before the flush's tick. The
encoder, which takes items in axis order across channels, promises with each item, so its blocks
close as they reach the target, and a sparse channel's one block at the finish.

**Flush.** `flush(before)` promises `before`, closes at `before` every channel's open block that
holds an item before it, and then writes one known-through record at `before`, after every block
record that flush closed. Items at or after `before` stay in the open blocks. A channel with
nothing before `before` in its open block writes no record; the known-through record says it holds
nothing new before `before`. A flush at the latest known-through tick writes nothing. A live recorder flushes at least every second and syncs the file to disk after
each, so how often it flushes, and the compression still pending, bound what a crash loses.

**Compression.** It compresses blocks at the level it is given, and overview blocks at zstd level 3,
whatever that level is. Strips and the annotation bundle are compressed at the finish. By default
the compression of every other frame is spread over the calls after its record is made, on the
caller's thread: a frame is cut into slices of 8 KiB of decoded bytes from its start, and while
any frame is pending, every call, `take_output` included, compresses the next slice, and the
slices after it while they fit in 8 KiB together, so no call does much work however large a block
is. Each slice ends a zstd block inside the frame, so the frame's bytes depend on the data, the
level and the slice, never on how the calls fall. Its records leave in file order: a record waits
behind the frames before it, so a flush's known-through record leaves only after the bytes of that
flush's blocks. A closed block reaches the output within one call for each slice still to compress
of the frames up to and including its own. A ready-made option compresses on other threads
instead, channels in parallel, and gives the same bytes. `finish` compresses whatever is still
pending.

**Memory.** It holds each channel's open block, each table's open pages at each depth (at most
4,096 entries each) and the top index's entries, each overview level's open block and open run, and
the frames waiting for compression. Each of those is bounded whatever the recording's length, so a
month-long recording holds a few open pages of index, not every block's row. Some fill slowly: a
coarse level's open block over days, a directory's open page over months, so the writer's memory
rises toward that bound for a long while. Only two things it holds grow with the recording. Every
gap and break stays until the finish, 24 bytes each: the content id hashes a list's count before
its entries, so their hash waits for the count. Every annotation it takes stays until the finish
too, whole and with its id: the finish writes the bundle or the generator files from them, and an
id its generator has given already is refused.

## Reading

### With random access

A reader reads the file's last 32 KiB first: over HTTP with `Range: bytes=-32768`, which a server
answers with the whole file when it is shorter. It reads the trailer, and fetches the rest of the
footer if the footer starts before those bytes. It parses the footer once, and decompresses the
strips of the channels it shows: that is a first draw. For a span of a channel it then picks a
level by the width of a column: the coarsest of the channel's levels whose bucket is no wider than a
column, or the channel's items where even its finest is wider. It fetches that table's pages
through the top index, directory pages first where the table has them, then the overview blocks or
blocks the span covers, as the rows place them, and those either side of the span, so a pan seldom
waits. It decodes each block once for a view, whatever channels the view takes from it.

### In order

A reader takes the records from the start. Each block record carries its channel, its span and its
word, so the reader decodes a channel's blocks without the footer, and each known-through record
extends every channel to its tick. It may skip pages and the annotation bundle, whose contents
follow from the records it reads; tickwave's reader checks each page against them instead, and
refuses one that differs by a byte from the page the records before it give. It stops at the first record that runs past the end of the file
or whose check does not match, and keeps every record before it. After the footer record come
exactly the trailer's 24 bytes.

A viewer reading a file that is still being written meets a last record cut off where the writer
has got to. It reads that record again once the file grows. The declaration fixes each channel's
finest level, so the viewer extends each channel's levels bucket by bucket as its blocks and the
known-through records arrive, and adds a coarser level once the length needs one.

## Repairing a cut-off file

A cut-off file is repaired by appending what a finished file has after its last good record:

1. Take the records up to the first bad one. Refuse a file with no declaration, or with neither a
   block nor an annotation record.
2. Where they hold a known-through record, cut the file just after the latest one: every channel is
   then known to its tick, and none holds more. Keep the annotation records that came after it, to
   write again after the cut. Where they hold none, each channel keeps its blocks to its last
   whole one, except that a derived channel's operands keep only the samples both hold: cut the
   file just after the last record at which every derived channel's operands hold as many
   samples, and keep the annotation records after the cut as above.
3. The length is the later of that known-through tick (or, with none, the latest end of any kept
   block, 0 with no block) and one past the latest start among the annotation records, so what a
   recorder marked after its last data survives, and a recording of marks alone too. Each channel
   whose last kept block ends before the length gets an empty block from there, or from tick 0, to
   the length.
4. Every digital channel's `end` is the source tick at the latest known-through tick, or, with
   none, at its own last kept block's end, or its start with no block, before the gap rule above. A
   file that can be repaired states no kept span, so its one source maps by `T = k + origin`, and
   that tick is the axis tick minus `origin`. No analog channel states a loss after its last
   sample.
5. Count each channel's edges and samples from its blocks' words, and take its gaps and runs from
   their records.
6. Compute each channel's overview and the content id from the kept blocks' items, decoding each
   block once. Keep the overview blocks and pages already written that the kept blocks give, and
   write the rest, the open runs included. Refuse a file whose overview blocks or pages differ from
   the ones its content gives.
7. Write the annotation records kept in step 2, every table's last pages, the annotation bundle if
   the writer would have bundled, the footer and the trailer, and otherwise each annotation
   source's generator file.

A repaired file holds what a writer that had finished right after the records repair keeps, with
the same length, would have written, and so has its content id.

A file whose channels state `kept`, because a channel is moved or cut, the recording merges several
sources, or its source states a stretch other than 1, is not repaired. Its declaration states each
channel's kept span before any data, to the end of the whole capture, so a footer at an earlier
tick would contradict it. Such a file is made again from its sources instead.

## Re-cutting

A file whose blocks a recorder closed early, at flushes, can be written again with blocks of the
usual size, and a repaired file too. A re-cutter reads the records in order and writes the same
declaration, then every channel's items, gaps and runs, and every annotation, in the same order,
and finishes with the same length, ends, losses, extras and bundled annotations. It may give the
channels other overview shifts than the input declares, such as the ones the rule gives each
channel's own items; a finished input's header says enough to work them out. It computes the
overview again. The new file has the same content id. A cut-off file re-cuts as repair would
finish it, its annotations bundled or handed back as generator files.
