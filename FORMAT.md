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

Every time a channel stores is in ticks of its source. A source's tick is its resolution: 1 ns for
a 1 GS/s logic analyser, one sample period for a sigrok capture, the timescale for VCD, one count
of a probe's stamping clock. Each source states its tick rate as an exact ratio. An edge, a gap's
ends and the tick an analog run starts at are whole ticks. An analog channel's samples sit on a grid
of their own: its run's start, plus the run's phase, a fixed fraction of a tick, plus a whole number
of periods, and a period can be a ratio (see Header). A channel holds its source's times as the
source gave them, so nothing in it is finer than the source stated, and no reading shows more
resolution than the source had.

A source's time `t`, in its own seconds, is at source tick `k = round((t − begin_s) × rate)`, where
`begin_s` is the source's own time at its tick 0 and `rate` its tick rate. A converter refuses a
source whose edges sit more than 0.01 of a tick off that grid, or whose samples sit more than that
off their own. Every source tick a channel stores lies in `0..2^53`.

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
source ticks can share an axis tick. A reader refuses a source whose `m_s` is below 1/2: a source's
own rate puts its `m` at 1 or more (see below), and its stretch lies in `1/2..=2`, so at most two of
its ticks share an axis tick.

A position that isn't a whole tick, an analog sample's place, maps by the same rule: `k` is then an
exact ratio, and `floor` takes the whole axis tick at or before it. Every such place is a whole
number of `1/D` ticks, where `D` is its channel's grid (see Header), so a reader multiplies through
by `D` and still computes in 128-bit integers: `T = origin_s + floor((k × D + offset_c × D) × a / (b
× D))`.

`m_s` is `(stretch num × R num × rate den) / (stretch den × R den × rate num)`. To find `a` and `b`,
a reader takes each of the nine pairs of a term on top and a term below, and divides both by their
greatest common divisor, before multiplying. The two products are then in lowest terms, and a
reader refuses a source whose `a` or `b` reaches 2^53. Three terms below 2^53 can multiply past 128
bits, so a reader multiplies with an overflow check: a product that overflows has passed 2^53.

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
| records | up to the trailer | the declaration, then blocks, known-through records, gaps, runs, overview blocks, pages, annotations and annotation parts as they come, then the annotation bundle and the footer |
| trailer | 24 bytes | where the footer is |

A finished file ends with the footer record and the trailer. A file without them was cut off or is
still being written (see Reading in order, and Repairing a cut-off file).

### Preamble

| Offset | Type | Field |
| --- | --- | --- |
| 0 | 8 bytes | magic: `89 54 4b 57 0d 0a 1a 0a` (`\x89TKW\r\n\x1a\n`) |
| 8 | u16 | major version, 1 |
| 10 | u16 | minor version, 0 |

A minor version adds header fields that older readers ignore, and record kinds from 128 up, which
they skip. It adds only what an older reader can leave out and still read everything else as the
writer meant: a field whose absence changes nothing else's meaning, a record nothing else depends
on. A change to how existing data reads goes behind a feature name in `requires` (see Header), which
a reader that doesn't know the name refuses, for the recording or for one channel. Any other change
is a major version: it changes the layout, and a reader refuses a major version it does not know.

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
| 8 | known through | a tick before which every channel's items are in |
| 10 | index page | one channel's block rows or overview rows, or a directory of pages |
| 11 | list page | one channel's gaps or breaks |
| 12 | annotation bundle | the sidecar's files but the parts' lines, bundled at the finish |
| 13 | annotation part | a run of one generator's annotations |

The declaration is the first record, and only the first. The footer is the last, and the trailer
follows it. The rules below say where the others go.

Kind 0 is never a record, so a tail of zeros, which a power cut can leave, stops a reader. A reader
refuses a record of a kind it does not know from 1 to 127, kind 9 included, and skips one from 128
to 255 anywhere after the declaration. A file whose first record is not the declaration is refused,
whatever that record's kind.

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
major version it does not know. A reader that reads the preamble too, in order or to repair,
refuses a trailer whose versions differ from it.

## Declaration

UTF-8 JSON: the header's `timebase`, `channels`, `derived`, `sources`, `title`, `meta`, `requires`
and `overview_shift`, as far as a writer knows them before any data.

A channel entry has the header's `name`, `source_name`, `source` and `kind`, and `offset`,
`offset_kind`, `kept`, `significant`, `overview_shift`, `codec` and `requires` where it states
them. A digital channel has `initial`, and an analog channel `unit`, `period`, `coding` and `phase`
where it states one. What only the end settles is left out: a digital channel's `end`, `edges` and
gaps, an analog channel's `first`, `count`, breaks and losses, and every channel's `blocks` and
`overview_blocks`. A writer that does not know a level at the channel's start (see Header) declares
it false and starts the channel with a gap from its start.

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
| `requires` | array of strings, optional | features a reader must understand to read the recording at all |
| `annotation_parts` | integer, optional | the number of annotation parts, at least 1, absent where there are none (see Annotations) |

A channel entry has `name`, `source_name` (the source's own name for it), `source` (its source's
index in `sources`) and `kind`, and `offset`, `offset_kind` and `kept` where it states them (see
Moved and trimmed channels). It can state `overview_shift`, its finest overview level's shift. It
has `blocks`, the number of its blocks, and `overview_blocks` where any of its overview levels go
in overview blocks: one `{"shift": s, "blocks": b, "rows": r}` a level, finest first, with `b` the
level's overview blocks and `r` its index rows (see Overview and Index). An analog channel can
state `significant`, an integer of at least 1: its readings carry at most that many significant
figures, as an instrument whose step changes with its range does.

`requires` names features a later version adds that change how data reads. This version defines
none, so its writers state none. A reader refuses a recording whose `requires` names one it doesn't
know. A channel entry, and a derived entry, can state `requires` too: a reader that doesn't know a
name there reads the recording without that channel. It shows the channel as unavailable, by name,
skips its blocks, overview and strip, shows every derived channel over it as unavailable too, and
leaves out every annotation that names it, saying which (see Unavailable channels). A channel entry
can state `codec`, how its blocks are stored. This version defines none, and a channel without one
stores its blocks as Blocks says. A reader treats a channel with a `codec` it doesn't know as
unavailable. No `requires`, `codec` or operation changes a channel's entries in the top index or
the headers of its records, so a reader finds and skips an unavailable channel's tables and records
by their layout alone. Nor does one change the JSON type of a field this version defines, so a
reader that doesn't know it still parses the entry.

- `"digital"`: `initial` (the level at the channel's start before any edge there, a boolean), `end`
  (a source tick: the level is known from the channel's start to `end`; a source can stop knowing
  one channel before another), `edges` (the count), and its gaps where the level was not known for
  a while (see Gaps, below). A channel's start is its kept span's `from` (see Moved and trimmed
  channels). In a recording that states no kept span it is the source tick at the axis's tick 0,
  `−origin`. An `end` at the channel's start says the level is never known. Such a channel has no
  edges and no gaps, and its `initial` is its declaration's, which says nothing of the level.
- `"analog"`: `unit` (empty when the source states none), `first` (the tick its first run starts
  at, 0 when there is no sample), `phase` (`[num, den]`, with `num` from 1 to `den − 1`, absent
  when 0),
  `period` (source ticks from one sample to the next, `[num, den]`), `count`, its breaks where the
  samples stopped for a while and its losses at either end (see Gaps), and `coding` (see Codings).

An analog channel's samples come in runs, `period` apart, each run starting a fraction of a tick,
its phase, after a whole tick. A channel without breaks has one run, and its sample `i` sits at
source tick `first + phase + i × period` exactly, its place, and on the axis at the tick its place
maps to. An instrument whose analog samples sit at a fixed offset from its digital channels' ticks,
smaller than a tick, states it as the phase. A phase is not a move: a move is a claim about the
channel's path (see Moved and trimmed channels), and a phase is where the instrument sampled.

A channel's grid `D` is the least common multiple of the denominators of its `phase` and its
`period`, in lowest terms, 1 where both are whole. Every place of the channel is a whole number of
`1/D` ticks, and so is every break's phase (see Gaps).

Every axis tick the header and the index name lies in `0..length`: every block's start. An
annotation part's row is the exception, since its part may be written before the length is known
(see Index). Every source
tick a channel names lies in `0..2^53` and maps into `0..length` (see Times): every edge and every
sample's place, every tick a gap or a break names, the start of a loss before the first sample, and
a kept span's `from`. A digital channel's `end`, a kept span's `to`, and the end of a loss after the
last sample map into `0..=length`. An analog channel has no `end`: its samples may stop before the
length. In a recording that states no kept span, the source's `origin` is at most 0, so each
channel's start maps to tick 0. `length` is at least 1. It is below 2^53, and so are the terms of
every `timebase`, `period` and `stretch`, the terms of each source's `m` in lowest terms, and every
`edges`, `count`, `blocks`, `rows`, break index and lost count. Every `origin`, `align` and `offset`
has a magnitude below 2^53. No term of a `timebase`, a `period` or a `stretch` is zero. A `period`
is at least 1, so no two samples of a run share a source tick. A `stretch` lies in `1/2..=2`. A
channel's grid `D` is below 2^16. Every `[num, den]` in the header, a coding's `scale` and `offset`
included, is in lowest terms with a positive `den`, so one value has one spelling and one content
id. A reader refuses a header that breaks any of these before it reads a block.

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
storing its samples. `derived` holds one entry each: `{"name": n, "unit": u, "op": o, "operands":
[a, b]}`, where `a` and `b` name analog channels, in order, and `o` is the operation. No two
channels or derived channels share a name, and a reader refuses a header where any do. Its operands
come from one source, and share `first`, `phase`, `period`, `count`, `breaks`, their losses at
either end and their move, the channel's `offset` (0 where a channel states none), so its sample i
sits where theirs does. A reader refuses operands that differ in any of these, rather than pair
samples from two grids. It compares what the header holds when it opens the recording, and the
operands' list pages wherever it fetches both, so it needn't fetch a list of breaks to draw a span:
the first operand's breaks are the derived channel's.

This version defines two operations:

- `"multiply"`, `a_i × b_i`, as power is current times voltage. When both are `int` channels whose
  codings state no `offset`, their `bits` sum to at most 64, and the magnitudes of their scales'
  numerators multiply to below 2^53 and so do their denominators, sample i is
  the integer product of their codes, signed if either is, with `scale` the product of their
  scales, exactly. Otherwise it is `f64(a_i) × f64(b_i)`, rounded once.
- `"subtract"`, `a_i − b_i`, of their values, as a differential probe's two inputs give one signal.
  For two `int` channels, let `s` be the greatest common divisor of their scales' magnitudes, as
  ratios: `gcd(|p| × v, |u| × q) / (q × v)` for scales `p/q` and `u/v` in lowest terms, itself in
  lowest terms. `ka = scale_a / s` and `kb = scale_b / s` are then whole numbers. The result is the
  code `ka × code_a − kb × code_b`, signed, with `scale` `s` and `offset` `offset_a − offset_b` (an
  `offset` a channel doesn't state is 0), and with `bits` the smallest that holds that code for
  every pair of codes the operands' codings allow. It is that integer where its `bits` is at most
  64 and every term of `s` and of the offset has a magnitude below 2^53. A reader that finds the
  code's range passing 128 bits takes the result as f64, since its `bits` passes 64. Otherwise,
  and for any other operands, sample i is `f64(a_i) − f64(b_i)`, rounded once.

In both, a `scale` a channel doesn't state is 1.

An `int` operand's f64 follows the rule for an `int` channel. An integer result's value as an f64
follows that rule too, and a reading of it the rule for an `int` channel; a reading of an f64
result follows the rule for a float. A derived entry can state `significant`, as an analog channel
can.

A later version can add operations, and an operation's further terms go in `params`, an object of
strings, so a reader that doesn't know the operation can still check the content id (see Content
id). The operations this version defines take no `params`, and a reader treats an entry of one of
them that gives some as unavailable, as it does an entry whose `op` it doesn't know (see
Unavailable channels). For such an entry it checks only that its name is unique, that it names at
least one operand and each names a channel, and that `params`, where stated, is an object of
strings. It can still find the channel's tables and strip, since a derived channel's finest level
follows from its operands' whatever its operation (see Overview), and skip them.

Blocks hold nothing for a derived channel. It has an overview of its own, so a reader draws it
zoomed out from the overview alone, and measures a span decoding only the operands' blocks at the
span's two ends. Its entry has `overview_blocks` as a channel's does. Derived channels are numbered
after the channels: derived channel `d` is overview channel `C + d`, where `C` is the number of
channels.

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
just before `from`. It keeps every analog sample whose place lies in the span, and each keeps its
place: a run that the cut starts inside starts at its first kept sample, at that sample's place, its
whole tick as the run's tick and the rest as its phase. Nothing moves forward, and no kept sample is
lost. That phase is the channel's declared one, so it sets the grid, and a later run's phase has to
be a whole number of the new grid's units. A converter refuses a cut where it isn't, which only a
run whose phase has a finer denominator than the period's and the cut's first phase can meet.

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

An analog channel's breaks are a list of `[index, tick, lost]` or `[index, tick, lost, phase]`:
sample `index` starts a new run, at source tick `tick` plus `phase / D`, and the instrument lost
`lost` samples before it, 0 when it only paused. `phase` is a whole number in `0..D`, where `D` is
the channel's grid, and 0 where the entry has three terms. The first run starts at `[0, first]`, at
the channel's `phase`. Sample `i` of the run that starts at `[index, tick]` with phase `f` sits at
source tick `tick + f / D + (i − index) × period` exactly, its place on the run's grid. The grid
goes on past the run's last sample, and before the first run's first sample. Each `index` lies in
`1..count` and is larger than the one before it. Each run's start is at least the place of the
sample before it, and where it states `lost` samples, past place `index` on the run before it, so
at least part of a lost sample lies before it. A run on an instrument whose sampling clock kept
running through a pause starts on the old run's grid, and with a period that isn't whole, its phase
can differ from the run before it. A reader draws no line across a break. The lost samples sit where
the run before the break would have put them: their time runs from place `index` on that run's grid
to place `index + lost`, and ends at the new run's start if that comes sooner. The rest of the break
is a pause. A reader shows the two apart.

An analog channel can also state samples the instrument lost before its first sample and after its
last, each at least 1, and absent when none:

- `lost_before`: they sit at places `−lost_before` to −1 on the first run's grid, so their time runs
  from the place of `−lost_before` to the first sample's place. A reader refuses a loss whose start
  lies below source tick 0 or maps before axis tick 0.
- `lost_after`: they sit at places `count` to `count + lost_after − 1` on the last run's grid, so
  their time runs from the place of `count` to the place of `count + lost_after`. A reader refuses a
  loss whose end maps past the length, and a writer refuses a length that doesn't cover it.

A reader pales those spans as it pales lost samples inside, and statistics count them as not
measured.

Each sample's time runs from its place to the next place on its run's grid, so inside a run it
reaches the next sample's place, and every sample of a run lasts one period. A run's last sample
ends at the earliest of the next place on its grid, the next run's start, its channel's kept `to`
where it states one, and the source time the length maps back to (below). A lost span is cut the
same way. These are exact times. All but the last are whole numbers of `1/D` source ticks.

On the axis a sample covers the ticks from the one its place maps to up to the one its end maps to,
which is what a reader draws. A reader that measures a span of the axis measures in exact time
instead, before any floor. The span's ends map back to source time exactly: axis tick `T` is source
time `(T − origin_s) / m_s − offset_c`. Each sample weighs the length of its time inside the span,
times `m_s`, so a stretch counts and the floor onto the axis changes nothing. A sample whose time
reaches into the span counts even when its place lies before it. So a measurement over two spans
side by side adds up to the measurement over both. With integer codes it is exact, in exact
rationals: with a stretch or a grid finer than a tick, its sums pass 128 bits.

A list of at most 64 entries is in the header: a digital channel's `gaps` and an analog channel's
`breaks`, each an array of the entries above. A longer list is in list pages (see Index), and the
header states its length instead, as `gap_count` or `break_count`. A channel with no gap or break
states neither. A reader refuses a list in the header of more than 64 entries, and a `gap_count` or
`break_count` of 64 or fewer.

## Blocks

A channel's items are cut by time into blocks of its own: a block holds exactly the channel's edges
or samples whose axis ticks lie in its span. A channel's blocks tile the axis in order: the first
starts at tick 0, each later one at the end of the one before, every one ends after it starts, and
the last ends at the length. Each channel's blocks are cut apart from every other channel's, so a
slow channel's block can span hours while a fast one's spans milliseconds.

A writer closes a channel's block once its decoded size reaches a target. It never closes one
between two of the channel's items at the same axis tick. A writer may also close a block before
it reaches the target, at an axis tick before which it has every item of the channel. A live
recorder does this so a crash loses little, and such a block can hold no item at all. A block
decodes to at most 4 MiB (see Limits), so a writer's target leaves room for the items it takes
before it can close.

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

Each block, overview block, strip, annotation part and annotation bundle is one standard zstd frame
with a content checksum, and nothing else: no skippable frame, no second frame, no byte after it,
and no dictionary. A reader refuses a frame without a checksum, with one that does not match, or
with anything else in its bytes. (A decoder that reads every frame it is given would check only the
last checksum, so a valid empty frame after a corrupted block would hide the corruption.)

A block's decoded payload is its channel's section, laid out as below for a channel that states no
`codec`:

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
record may start before its tick, as a recorder that reports a loss when it ends writes one, but its
`to` maps to that tick or after it, since the overview before it may already be written. Each
known-through record's tick is later than the one before it, and no later than the length. Every
block record before it ends at or before its tick, so a file cut just after it holds nothing past
its tick (see Repairing a cut-off file). A reader refuses a known-through record that a block record
before it ends after, a block record after it that holds an item before its tick, and a gap record
after it whose `to` maps before its tick.

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

A digital channel's gap, as Gaps defines one. A channel's gap records come in time order: each
one's `from` lies past the `to` of the one before it, and no edge of the channel lies in its
`from..=to`. It comes before the channel's block that holds the axis tick `to` maps to. The
channel's blocks before that one may already be written when the writer learns of the gap. They
stay as they are, because a block's level bit carries the level from before a gap until the gap's
`to`. A reader refuses a gap record out of order, one that an edge contradicts, and one whose `to`
maps before the end of the channel's last block record before it.

A run record:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | u64 | `index` |
| 12 | i64 | `tick`, a source tick |
| 20 | u64 | `lost` |
| 28 | u32 | `phase`, in `1/D` ticks: below `D`, the channel's grid |

Sample `index` of an analog channel starts a run at `tick` plus `phase / D`, after `lost` samples
the instrument lost. The run at index 0 gives the channel's `first`, its phase is the declared
`phase`, and its `lost` is the channel's `lost_before` (see Gaps); each later run is a break, with
its phase. A channel's run records come in order of `index`, the first at 0 and each later one
larger than the one before it, and each starts where Gaps says a run may. It comes before the
channel's block that holds sample `index`. A reader refuses a run record out of order, one with a
`phase` of `D` or more, one at index 0 whose phase isn't the declared one, one that starts where
Gaps says a run may not, and one whose `index` is below the samples the channel's block records
before it hold.

The header's gaps and breaks are these records, in the order they came, but for the ones left out.
A gap whose `to` is not below its channel's `end` is left out of the header, and the channel's
`end` becomes the gap's `from` if that comes sooner. Its `to` may lie anywhere past the `end`, the
length's tick or past it included: a recorder that stops inside a loss reports the loss up to
where it stopped. A gap from the channel's start then leaves its `end` at its start: the level is
never known. A run whose `index` is not below its channel's `count` is left out too.

So a writer can't know that a channel's last gap or run is listed until another comes, the gap's
block closes or the run's first sample comes, or it finishes. One that hashes the lists as they
come (see Content id) holds back what it can't yet know. A page of a gap list comes after the
channel's block that holds the axis tick of its last gap's `to`, and a page of a break list after
the block that holds its last break's sample, so no page written holds an entry that is left out.

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

A derived channel's finest level is its operands' coarser one whatever its operation, so a reader
lays out every derived channel's tables without knowing its operation.

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

- **Digital.** n density bytes, then `ceil(n / 2)` duty bytes. Together they say how busy the
  channel is in each bucket, and whether it spends its time high or low there, for a view too
  coarse to draw edges. Neither is exact: the edges are in the blocks.

  A bucket's density is 0 when no edge falls in it, and otherwise `1 + floor(4 × log2(e))` for its
  `e` edges: one step every quarter of a doubling, so a bucket with twice as many edges is four steps
  up, at any zoom and in any recording. Counts are below 2^53, so a density is at most 212. Small
  counts are exact: 1, 5 and 7 are one, two and three edges. A gap's ends are not edges; two edges at
  one tick are two. A reader computes it in integers, since a floating-point `log2` lands on the
  wrong side of a step for some counts below 2^53: with `x = e << (63 − floor(log2 e))` as a u64,
  the density is `1 + 4 × floor(log2 e)` plus one for each of `0x9837f0518db8a970`,
  `0xb504f333f9de6485` and `0xd744fccad69d6af5` that `x` is at least: the thresholds
  `ceil(2^(63 + r/4))` for r = 1, 2, 3.

  A bucket's duty is 4 bits. Of the bucket's axis ticks, `K` lie in the channel's kept span: at or
  after the tick its start maps to, and before the tick its kept `to` maps to, or before the length
  where it states no kept span. `H` of those are high: the level at a tick is `initial` taken
  through every edge and gap end at or before it, as the block words take it, so inside a gap, and
  past the channel's `end`, the level from before counts. The duty is 15 when `K` is 0, so 15 says
  the bucket lies wholly outside the channel's kept span, and otherwise `14 × H / K` rounded to the
  nearest whole number, a half rounding up: `floor((28 × H + K) / (2 × K))`, in 128-bit integers. A
  bucket wholly low reads 0, and one wholly high 14. Bucket `2j` is the low 4 bits of duty byte `j`
  and bucket `2j + 1` the high 4 bits; with n odd, the last byte's high 4 bits are 0.

  Duty takes no account of gaps or of the channel's `end`, because a recorder reports a loss when
  it ends: a gap can start in a bucket a writer has already written, and a loss still running when
  the recorder stops moves the channel's `end` back over such buckets. A reader shows gaps from the
  channel's gap list, and the span past its `end` as not known, instead.

  A bucket of the finest level counts its edges and ticks. A bucket of a coarser level takes the
  sums of the 16 buckets of the level below it that it covers, edges, `K` and `H` alike, and
  computes its density and duty from them, never from theirs, so every writer gives the same
  bytes. A writer keeps those sums while it builds; the file doesn't store them.
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
  bytes. An integer sum is exact. A bucket's sample count follows from `first`, `phase`, `period`,
  `breaks` and the mapping, so the overview doesn't store it.
- **Derived.** As an analog channel, in the coding its operation gives (see Derived channels): an
  integer product as signed or unsigned `int` codes whose `bits` are its operands' summed, an
  integer difference as signed `int` codes of its `bits`, and an f64 result as `f64`.

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
The records of one channel's level come in order of `k`. Those of different channels or different
levels keep no order among themselves: a writer may settle one channel's levels before another's.

## Annotations

An annotation record's payload is a u16, the length of `s`, then `s`, then `a`. `s` is the
generator's name in UTF-8, as a generator file's first line gives it, and `a` is one line of that
file ([ANNOTATIONS.md](ANNOTATIONS.md)): a UTF-8 JSON object, ending with a newline and holding no
other. Its times are axis ticks in `0..2^53`. A reader refuses a record whose `s` is not a name
ANNOTATIONS.md allows a generator, since the source's file is named after it, and one whose `a`
holds a newline before its end or is not a JSON object. tickwave's writer refuses, at the call, an
annotation whose fields, ticks or channels break ANNOTATIONS.md's rules for a line, and writes the
rest as compact JSON. Its length aside, which the writer learns only when it finishes, each
annotation it takes resolves. Another writer may write a line that breaks those rules. A re-cutter
leaves such a line out wherever it lies, and repair among the records it writes again after its cut;
each names what it leaves out. A record before repair's cut stays where it is, since the part after
it covers it. Annotations keep no order with the channels' items.

Annotation records let a live recorder keep what it generates as safely as its data. The footer
does not index them. A file that holds them ends with them bundled, in annotation parts and an
annotation bundle record, so a live recording stays one file, and a reader with random access reads
them from there.

An annotation part holds a run of one generator's lines. Its record's payload is a u32, the decoded
length, then one zstd frame that decodes to a u16, the length of `s`, then `s`, the generator's
name, then the lines, each ending with its newline. A part covers the annotation records of its
generator that lie between that generator's previous part and itself in the file, and holds their
lines exactly: byte for byte, in the order the records came. Where no record of its generator lies
between, the part holds lines of its own. A re-cutter and `tickwave bundle` write a file's
annotations that way, as parts and no annotation records, since a finished file needs none. A reader
in order checks each part that covers records against them. A reader refuses a part with no line,
and one with a line that has no `start` or a time that doesn't convert to ticks, since its row
couldn't state the span. Repair and re-cutting take a
generator's annotations from its parts and from its records after its last part, never from a record
a part covers, so no annotation comes out twice. The annotation parts table (see Index) gives each
part's place and the span its annotations cover, so a reader fetches only the parts a view reaches,
and a long recording's annotations load as its data does.

A writer writes a generator's part once the lines since its last part reach a size it picks, after
the record whose line takes them there. At the finish it writes each generator's rest as a part,
the generators in the order of their first annotations. A re-cutter writes parts that stand alone
the same way, as it reads the lines, and carries its input's bundle. A writer that bundles a
sidecar's files given whole, as `tickwave bundle` does, splits each generator file's lines into
parts in the file's order, and writes them at its finish, the generators in the order the
hand-written file includes their files.

The annotation bundle record holds the sidecar's files without the parts' lines. Its payload is a
u32, the decoded length, then one zstd frame of UTF-8 JSON: an object of strings, each file's name
and text, where a generator file's text is its first line alone, with its newline. The whole
generator file is that line, then the lines of every part whose generator is its own, in the order
of the parts' rows. A live recording's hand-written file holds no annotation of its own and includes
the generator files, in the order of their generators' first annotations. A file holds at most one
bundle record, written just before the footer, and the footer's top index places it. A recording
with annotation parts has a bundle record, and each part's generator has a file in it; a reader
refuses a recording that breaks either. A reader that wants the sidecar's files whole, to write them
beside the recording, joins them back from the bundle and the parts. It leaves out and cuts
annotations at the length as below while it joins, so the files it writes pass ANNOTATIONS.md's
checks.

A reader leaves out an annotation that starts at or past the length, and ends a span that ends past
it at the length. A generator gives each annotation an id of its own (ANNOTATIONS.md). tickwave's
writer doesn't check that, since it would have to hold every id: its producer gives unique ids. A
reader that meets a second annotation with an id its generator has given already keeps the one that
comes first in the file's order, parts in the order of their rows and lines in order, and reports
the other. A reader that fetches only the parts a view reaches can't see that a part it hasn't
fetched repeats an id. No row indexes ids, so a reader resolves a hand-written annotation's `ref`
once it has read every part.

## Index

Each channel's blocks, each level of a channel's overview in overview blocks, each gap or break
list longer than 64 entries, and the recording's annotation parts are a table each: its entries in
order. A table's entries go in pages of 4,096, the last page the rest, and the footer's top index
lists the pages. Past 16 pages, directory pages list the pages, 4,096 entries a directory page, and
the top index lists the directory pages; past 16 of those, another directory level goes above, and
so on. So the top index holds at most 16 entries a table however long the recording, and a reader
holds one page a level at a time.

| Table | Its entries | Their count |
| --- | --- | --- |
| a channel's blocks | block rows | the channel's `blocks` |
| one level of a channel's overview in overview blocks | overview rows | the level's `rows` |
| a digital channel's gaps, past 64 | gaps | `gap_count` |
| an analog channel's breaks, past 64 | breaks | `break_count` |
| the recording's annotation parts | part rows | `annotation_parts` |

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

A gap is 17 bytes, `from` (i64), `to` (i64) and `level` (u8); a break is 28 bytes, `index` (u64),
`tick` (i64), `lost` (u64) and `phase` (u32), as Gaps defines them.

A part row, 32 bytes:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u64 | the offset of its zstd frame in the file |
| 8 | u32 | the frame's length |
| 12 | u32 | its decoded length |
| 16 | i64 | `start`, the earliest axis tick any of its annotations starts at |
| 24 | i64 | `end`, the latest any of them ends at, a point ending where it starts |

The rows go in the order the parts were written. A part's `start` and `end` say which views it can
reach, so a reader fetches the parts whose span meets a view's: those with `start` before the view's
end and `end` at or after its start. They lie in `0..2^53`, `start` at most `end`, and may pass the
length, since a live writer writes a part before it knows the length. The rows don't go in order of
time, since a long span can start early in a late part, so the directory entries above them say
which times each page reaches instead (below).

A page record's payload, for an index page (kind 10) and a list page (kind 11) alike:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel, a derived channel numbered after the channels; 0 for the annotation parts |
| 4 | u8 | table: 0 blocks, 1 an overview level, 2 gaps, 3 breaks, 4 annotation parts |
| 5 | u8 | the overview level's shift, 0 for any other table |
| 6 | u8 | depth: 0 for a page of the table's entries, `d` for a directory of pages of depth `d − 1` |
| 7 | u64 | its first entry's place among the entries at its depth, counted from 0 |
| 15 | u32 | the number of its entries, 1 to 4,096 |
| 19 | the rest | its entries |

A list page holds gaps or breaks at depth 0; an index page holds block, overview or part rows at
depth 0, or directory entries at any depth from 1. A directory entry, 28 bytes:

| Offset | Type | Field |
| --- | --- | --- |
| 0 | i64 | `first`, of the first entry under it |
| 8 | u64 | `number`, of the first entry under it |
| 16 | u64 | the page record's offset in the file |
| 24 | u32 | the page record's length, its framing included |

`first` and `number` say where the page it points at starts: for blocks, the first block's start
tick and its first item's index in the channel; for an overview level, the first row's `k` and its
place among the rows; for gaps, the first gap's `from` and its place in the list; for breaks, the
first break's `tick` and its `index`. For annotation parts they say which times the page reaches
instead: `first` is the earliest `start` of the rows under it, and `number` the latest `end`, so a
reader skips every page whose span misses a view's. The top index's entries are directory entries
too.

A page record's length is 32 bytes plus its count of entries times their size: its table's own
entry's at depth 0, and a directory entry's, 28 bytes, above. The table's shape gives each page's
count (below), so a reader refuses a directory entry whose length differs from that before it
fetches the page.

A table's shape follows from its count `R`. Its depth-0 pages are `ceil(R / 4,096)`. Where the pages
at a depth number at most 16, the top index lists them, and that depth is the table's. Otherwise
directory pages one depth up list them, 4,096 entries a page in order, the last the rest. Counts
are below 2^53, so a table has at most four directory levels: pages alone list 2^16 entries, one
directory level 2^28, two 2^40, three 2^52, and a fourth every count past that. A reader refuses a
page whose channel, table, shift or depth differ from the entry that points at it, whose first place
or count differ from what the shape gives, or whose entries are out of order; a directory entry
whose `first` or `number` differ from its page's; and a top index whose depth or entry count for a
table differ from what the table's count gives.

A writer writes a table's page once it holds 4,096 entries, after the records its entries place,
and a directory page once it holds 4,096 entries likewise. A page of gaps or breaks waits for the
block that Gap and run records names. It writes every table's last pages at the finish.

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
| 12 | u8, u8, `e` entries | where the header states `annotation_parts`, the annotation parts table: its depth, `e` (1 to 16), then `e` directory entries |
| after it | the rest | an entry for each channel, in channel order, then each derived channel |

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

It is BLAKE3 over the timebase (two u64), the length (i64), each channel's own BLAKE3 digest, in
channel order, and then each derived channel's digest, in order. A channel's digest covers what the
declaration fixes, then the channel's items in order, then what only the end settles, so a writer
computes it as the items arrive. A channel's gaps or breaks, as the header lists them, go into a
digest of their own, and the channel's digest takes their count and that digest at the end, so a
writer holds none of them to hash them, but the one it can't yet know is listed (see Gap and run
records). Every tick in it is a source tick, as the channel stores it. Strings are
written as a u64 byte length and their UTF-8 bytes.

Between what the declaration fixes and the items, a channel adds a byte of flags where any is set:
1 if it states `offset`, 2 if it states `kept`, 4 if the recording has several sources or the
channel's source has an `m` other than 1 or an `origin` other than 0. Then `offset` as i64 and
`offset_kind` as a string, where it states them; `kept`'s `from` and `to` as i64, where it states
it; and with flag 4, its source's `m` as two u64 in lowest terms, `origin` as i64, `align` as i64,
0 where the source states none, and `stretch` as two u64 in lowest terms, 1 and 1 where the source
states none. A channel with no flag adds nothing, so a recording converted whole has the id its
data alone gives.

- Digital: the name, `"digital"`, the initial level as one byte, then every edge's source tick as
  i64, then `end` as i64, the number of gaps as u64, and the gaps' digest: BLAKE3 over each gap's
  `from` and `to` as i64 and its `level` as one byte, in order.
- Analog: the name, `"analog"`, the unit, `period` as two u64 and `phase` as two u64, both in
  lowest terms (0 and 1 for no phase), then the samples, then `first` as i64, `count` as u64, the
  number of breaks as u64, and the breaks' digest: BLAKE3 over each break's `index` as u64, `tick`
  as i64, `lost` as u64 and `phase` as u32, in order. Where the channel states `lost_before` or
  `lost_after`, both follow as u64, 0 for one it doesn't state. The samples of an `f32` or a
  `codes` channel are every value's f32 bits as u32, so the two codings of one capture's values
  give one id. An `f64` channel adds `"f64"` before the flags and hashes every value's bits as u64.
  An `int` channel adds, in the same place, `"int"`, `bits` as one byte, `signed` as one byte, and
  `scale` and `offset` as an i64 and a u64 each (1/1 and 0/1 where it states none), and hashes
  every code as i64 or u64.
- Derived: the name, the unit, the `op`, the number of operands as u64, each operand's name in
  order, then the number of `params` as u64 and each one's key and value, in the byte order of the
  keys. All are strings, so a reader checks a derived channel's part of the id whether or not it
  knows the operation. So changing an operation, or swapping the operands of a subtraction, changes
  the id, and so does editing the recipe an annotation pinned.

`significant`, `overview_shift`, `codec`, `requires`, and a source's fields other than the four
flag 4 adds are not part of the content: they describe it, say how a file stores it, or follow from
it. A source's `timebase` follows from its `m`, its `stretch` and the recording's `timebase`.

## Limits

A writer stays within these, and a reader opens anything within them. A reader refuses a file past
any of them, as an error it reports, before it fetches or allocates for the part that passes it:

- A block decodes to at most 4 MiB, 2^22 bytes. So a block holds at most 2^22 edges, since each
  takes a byte at least, or 2^22 samples.
- An overview block holds at most 4,096 buckets and a strip at most 1,024, at most 32 bytes a
  bucket (see Overview).
- An annotation bundle decodes to at most 64 MiB, and an annotation part to at most 4 MiB. An
  annotation record's payload is at most 4 MiB too, so a part of its line alone fits. A recording
  has at most 256 generators. The annotations themselves have no limit: there can be as many parts
  as it takes.
- A record of a kind this version defines is no longer than its kind holds. A gap record's payload
  is 21 bytes, a run record's 32 and a known-through record's 8, exactly. A block, overview block,
  annotation part or bundle record holds its fields and a frame within the bound below. A page is
  what its count gives. The declaration is at most 64 MiB, as the header is. A reader refuses a
  record whose `n` passes that before it reads the payload. A record of a kind from 128 up can be
  any length: a reader that skips one hashes it as it reads, without holding it.
- The footer record is at most 256 MiB, its header at most 64 MiB of it. A recording within the
  other limits needs less: its top index and strips come to under 48 KiB a channel.
- A recording has at most 4,096 channels and derived channels together.
- A frame is no longer than zstd's bound for its decoded length, `ZSTD_COMPRESSBOUND`: no writer
  needs more, so a reader refuses a row or a record whose frame length passes it before fetching
  the frame.
- A page's length is what its count gives (see Index).
- An analog channel's grid `D` is below 2^16, its `period` at least 1, and a source's stretch lies
  in `1/2..=2` (see Header).

These bound what any one part of a file can make a reader decode or allocate, whatever the file
states: a block's edges, held as 8-byte ticks, take at most 32 MiB. A reader on a smaller machine
can hold less, and then reports each part it can't take as an error, rather than failing.

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
- Writes annotation records as it generates them, and each generator's annotation part once its
  lines since the last one reach the size it picks (see Annotations).
- Writes a known-through record at a tick before which every item of every channel is in a block
  record before it.
- Writes overview blocks as their buckets settle. Block k of a channel's level settles once the
  channel is known past axis tick `4,096 (k + 1) × 2^shift`: one of its blocks ends there or past
  it, or a known-through record's tick does. A run of alike blocks is written when it ends: when a
  block with other bytes settles, or at the finish.
- Writes each table's pages as they fill (see Index).
- At the end, closes every channel's last block at the length, then writes a known-through record
  at the length if it has written any (so a file cut off inside its end repairs to itself), each
  overview level's last block and its open run, each generator's last annotation part, every
  table's last pages, the annotation parts table's included, then the annotation bundle where it
  took any annotation, the footer and the trailer.

A writer refuses, as an error, any tick, count or length that a reader would refuse, the axis tick
an item maps to included, and anything past the limits (see Limits), rather than write a file no
reader opens.

### tickwave's writer

**Output.** It hands its output to the caller as bytes, and counts every offset it writes, in rows
and pages, from its first output byte, so the caller writes the output from the start of the file.

**Order and promise.** It takes each channel's items in that channel's order, and refuses an item
before the start of its channel's open block. A promise, `promise(before)`, says every item of
every channel before axis tick `before` is in; `promise_channel(c, before)` says it of channel `c`
alone. It refuses an item before the latest promise to its channel. A channel's open block closes
once its items before the latest promise to it and to its partners reach the target size, the gaps
it has taken that end before that tick counted at 17 bytes each: those items go in the closed
block, and those at or after that tick stay in the open one. A derived channel's
operands are partners: they close their blocks together, at one tick. So a producer can write a
channel ahead of what it has promised, as a probe's samples come ahead of the pin edges that lie
among them. Without a promise, a block closes only at a flush or the finish. Promises and flushes
go forward: each is at or after the one before, so every block a writer closes before a flush ends
at or before the flush's tick, which repair relies on. The encoder, which takes items in axis order
across channels, promises with each item, so its blocks close as they reach the target, and a
sparse channel's one block at the finish.

**Limits.** Its target is at most 2 MiB, half the most a block can hold, 1 MiB unless it is given
another. It refuses an item that would take a channel's open block past 4 MiB where the block can't
close first: where closing at the latest promise to the channel and its partners would still leave
too much, because the items lie at one axis tick or no promise has come past them. The producer then
promises or flushes, and the item can go in. A flush always closes at its own tick, so a flush never
meets this limit. It refuses a sample of a derived channel's operand that would leave more than 4
MiB of that operand's samples, in its coding, waiting for the other operand's: an operand can run
ahead of its partner by at most that much.

**Flush.** `flush(before)` promises `before`, closes at `before` every channel's open block that
holds an item, or the end of a gap it has taken, before it, and then writes one known-through record
at `before`, after every block record that flush closed. Items at or after `before` stay in the open
blocks. A channel with nothing before `before` in its open block writes no record; the known-through
record says it holds nothing new before `before`. A flush at the latest known-through tick writes
nothing. A live recorder flushes at least every second and syncs the file to disk after each, so how
often it flushes, and the compression still pending, bound what a crash loses.

**Compression.** It compresses blocks at the level it is given, and overview blocks at zstd level 3,
whatever that level is. Annotation parts are compressed at the level it is given, strips and the
annotation bundle at the finish. By default the compression of every other frame is spread over the
calls after its record is made, on the caller's thread: a frame is cut into slices of 8 KiB of
decoded bytes from its start, and while
any frame is pending, every call, `take_output` included, compresses the next slice, and the
slices after it while they fit in 8 KiB together, so no call does much work however large a block
is. Each slice ends a zstd block inside the frame, so the frame's bytes depend on the data, the
level and the slice, never on how the calls fall. Its records leave in file order: a record waits
behind the frames before it, so a flush's known-through record leaves only after the bytes of that
flush's blocks. A closed block reaches the output within one call for each slice still to compress
of the frames up to and including its own. A ready-made option compresses on other threads
instead, channels in parallel, and gives the same bytes. `finish` compresses whatever is still
pending.

**Memory.** It holds each channel's open block, at most 4 MiB, each table's open pages at each depth
(at most 4,096 entries each) and the top index's entries, each overview level's open block and open
run with their sums, each derived channel's waiting samples, at most 4 MiB, each generator's lines
since its last annotation part, under 64 KiB and one line, each channel's gaps whose blocks are
still open, at most 4 MiB of them, the frames waiting for compression, and the content id's running
digests. Each of those is bounded whatever the recording's length, so a month-long recording holds
a few open pages of index, not every block's row, gap or annotation. Some fill
slowly: a coarse level's open block over days, a directory's open page over months, so the writer's
memory rises toward that bound for a long while.

**Gaps and runs.** It writes a run's record with the run's first sample, so it leaves out no run.
It hashes a gap, and adds it to its list's open page, once the channel's block that holds the gap's
`to` closes. It refuses a gap that would leave more than 4 MiB of gaps waiting, at 17 bytes each,
as it refuses an item past its limit; the producer then promises or flushes. At the finish it
hashes a gap still waiting unless the channel's end leaves it out, and refuses an end that would
leave out a gap it has hashed.

**Annotations.** It writes a generator's annotation part once that generator's lines since its last
part reach 64 KiB, after the record whose line takes them there. Before a line that would take them
past 4 MiB, it writes the lines it holds as a part first. At the finish it writes each generator's
rest, then the bundle record, whose hand-written file includes the generator files. It doesn't check
a generator's ids for duplicates. Nothing it does at the finish reads back what it wrote.

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
waits. It decodes each block once for a view, whatever channels the view takes from it. It checks
each length a row or a directory entry states against the limits before it fetches (see Limits).

For annotations it reads the bundle record and the annotation parts table's rows, and fetches the
parts whose span meets the view, as it fetches blocks. A reader that only draws the data may leave
annotations until after the first draw.

### Unavailable channels

A channel is unavailable to a reader when its `requires` names a feature the reader doesn't know,
its `codec` is one it doesn't know, or, for a derived channel, its `op` is one it doesn't know or an
operand is unavailable. The reader still opens the recording. It keeps the channel's place and name,
so every other channel keeps its number, and shows the channel as unavailable rather than leaving it
out. It skips the channel's blocks, overview and strip; the top index still says where they are, so
it reads past them, and it doesn't check the size of a strip whose layout it can't know. It leaves
out every annotation that names an unavailable channel in `channels` or `measure`, even one that
names other channels too, and says which annotations it left out and why. An annotation without
`channels` names no channel, and stays.

### In order

A reader takes the records from the start. Each block record carries its channel, its span and its
word, so the reader decodes a channel's blocks without the footer, and each known-through record
extends every channel to its tick. It may skip pages, and annotation parts that cover records,
whose contents follow from the records it reads; tickwave's reader checks each against them instead,
and refuses one that differs by a byte from what the records before it give. A part that stands
alone, and the annotation bundle, follow from no record: the bundle holds the hand-written file and
each generator file's first line. The reader refuses each record that breaks a rule this document
gives for records in order. It stops at the first record that runs past the end of the file or whose
check does not match, and keeps every record before it. After the footer record come exactly the
trailer's 24 bytes.

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
   write again after the cut. An annotation part after the cut that covers records goes, and its
   lines come back from the records; one that stands alone is kept, to write again. Keep, too, each
   gap record after the cut whose `from` maps before the known-through tick, to write again: a loss
   the recorder had begun to report then ends its channel at that `from` (see Gap and run
   records), rather than being lost with the cut. A record only a finish writes goes wherever it
   lies, since a file cut there was cut inside its finish and step 7 writes it again: a table's
   last page, one of fewer than 4,096 entries, and the annotation bundle. Where they hold none,
   each channel keeps its blocks to its last whole one, except that a derived channel's operands
   keep only the samples both hold: cut the file just after the last record at which every derived
   channel's operands hold as many samples, and keep the annotation records and parts after the
   cut as above.
3. The length is the later of that known-through tick (or, with none, the latest end of any kept
   block, 0 with no block) and one past the latest start among the annotations kept, so what a
   recorder marked after its last data survives, and a recording of marks alone too. Each channel
   whose last kept block ends before the length gets an empty block from there, or from tick 0, to
   the length.
4. Every digital channel's `end` is the source tick at the latest known-through tick, or, with
   none, at its own last kept block's end, or its start with no block, before the gap rule, which
   the gap records kept in step 2 may then apply. A file that can be repaired states no kept span,
   so its one source maps by `T = k + origin`, and that tick is the axis tick minus `origin`. No
   analog channel states a loss after its last sample.
5. Count each channel's edges and samples from its blocks' words, and take its gaps and runs from
   their records.
6. Compute each channel's overview and the content id from the kept blocks' items, decoding each
   block once. Keep the overview blocks and pages already written that the kept blocks give, and
   write the rest, the open runs included. Refuse a file whose overview blocks or pages differ from
   the ones its content gives. Each channel's levels are compared on their own, in order of `k`,
   whatever order the records of different channels and levels came in.
7. Write the records kept in step 2 after the cut, in the order they came. Before a part that stands
   alone, write a part of the lines of its generator's records written again before it, so that it
   still stands alone. Then write what a writer's finish writes. For each generator, in the order of
   its first annotation, that is parts of the lines of its records after its last kept part, each
   within the limits. A record a kept part covers is not written into another part. Then come every
   table's last pages, the annotation bundle, the footer and the trailer.

A repaired file holds what a writer that had finished right after the records repair keeps, with
the same length, would have written, and so has its content id.

A file whose channels state `kept`, because a channel is moved or cut, the recording merges several
sources, or its source states a stretch other than 1, is not repaired. Its declaration states each
channel's kept span before any data, to the end of the whole capture, so a footer at an earlier
tick would contradict it. Such a file is made again from its sources instead.

## Re-cutting

A file whose blocks a recorder closed early, at flushes, can be written again with blocks of the
usual size, and a repaired file too. A re-cutter reads the records in order and writes the same
declaration, then every channel's items, and the gaps and runs its header lists, in the same order,
and finishes with the same length, ends, losses, extras and annotations. It writes the annotations
as parts that stand alone, with no annotation records (see Annotations). It may give
the channels other overview shifts than the input declares, such as the ones the rule gives each
channel's own items; a finished input's header says enough to work them out. It computes the
overview again. The new file has the same content id. A cut-off file re-cuts as repair would finish
it.
