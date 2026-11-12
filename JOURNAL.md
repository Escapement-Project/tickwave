# The tickwave journal, version 1.0

A journal is the file a recorder writes while it captures. The recorder only ever appends to it.
A journal cut off at any byte, by a crash, a power cut or a full disk, still reads up to its last
whole record. It converts to a tickwave recording ([FORMAT.md](FORMAT.md)) that holds everything
up to there.

A journal's blocks are recording blocks. Conversion copies them as they are, and recompresses only
the short blocks a time limit closed.

Status: draft, with the recording format.

A journal is named like the recording it becomes: `rec.tickwave-journal` converts to
`rec.tickwave`.

## Layout

All numbers are little-endian. A journal is a preamble, then records, one after another, to the
end of the file.

### Preamble

| Offset | Type | Field |
| --- | --- | --- |
| 0 | 8 bytes | magic: `89 54 4b 4a 0d 0a 1a 0a` (`\x89TKJ\r\n\x1a\n`) |
| 8 | u16 | major version, 1 |
| 10 | u16 | minor version, 0 |

A reader refuses a major version it does not know.

### Records

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u8 | kind |
| 1 | u32 | `n`, the payload's length |
| 5 | `n` bytes | payload |
| 5 + n | 8 bytes | check: the first 8 bytes of the BLAKE3 hash of the kind, `n` and the payload |

A reader takes the records in order. It stops at the first one that runs past the end of the file
or whose check does not match, and keeps every record before it. Kind 0 is never a record, so a
tail of zeros, which a power cut can leave, stops a reader too. A converter says how many bytes it
left unread.

A reader refuses a record of a kind it does not know from 1 to 127, and skips one from 128 to 255.
A minor version adds kinds only from 128 up.

| Kind | Record | Holds |
| --- | --- | --- |
| 1 | head | what the recording is, before any data |
| 2 | block | one recording block |
| 3 | gap | a digital channel's gap |
| 4 | break | an analog channel's break |
| 5 | annotation | one generated annotation |
| 6 | end | the recording ended |

### Head

The first record, and only the first. Its payload is UTF-8 JSON: the recording header's fields
that are known before the capture starts, and two settings the recorder chooses, or lets its
user set.

| Field | Meaning |
| --- | --- |
| `timebase`, `channels`, `source`, `title`, `meta` | as in the recording header |
| `block_bytes` | the decoded size at which the recorder closes a block |
| `time_limit_ms` | the longest the recorder keeps a block open, in milliseconds of its own clock |

A channel entry leaves out what conversion counts. A digital channel has no `end`, `edges` or
`gaps`, and an analog channel no `count` or `breaks`. An analog channel's `first` and `coding` are
there: the recorder declares a coding before it has a sample, so conversion never picks one.

### Block

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u8 | why the block closed: 0 its size, 1 the time limit, 2 the recording's end |
| 1 | i64 | start tick |
| 9 | i64 | end tick, the next block's start |
| 17 | u32 | decoded length |
| 21 + 4c | u32 | channel c's count, as the recording index has it (a digital channel's level bit included) |
| 21 + 4C | the rest | the block: one zstd frame, as a recording block |

`C` is the number of channels. The first block starts at tick 0, and each later one at the end of
the block before. Every block ends after it starts. A block holds every item of every channel at
ticks `start..end`, and the recording format's rules for a block, its
counts and its level bits all hold. The recorder sets the end to a tick before which it has every
item of every channel. A block the time limit closed can hold no item at all.

Only the last block closes at the recording's end.

### Gap

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | i64 | `from` |
| 12 | i64 | `to` |
| 20 | u8 | `level`, 0 or 1 |

A digital channel's gap, as the recording format defines one. It comes before the block that holds
tick `to`. The blocks before that one may already be written when the recorder learns of the gap.
They stay as they are, because a block's level bit carries the level from before a gap until the
gap's `to`.

### Break

| Offset | Type | Field |
| --- | --- | --- |
| 0 | u32 | channel |
| 4 | u64 | `index` |
| 12 | i64 | `tick` |

An analog channel's break, as the recording format defines one. It comes before the block that
holds sample `index`.

### Annotation

UTF-8 JSON: `{"source": s, "annotation": a}`. `s` names the generator, as a generator file's first
line does, and `a` is one line of that file ([ANNOTATIONS.md](ANNOTATIONS.md)). Its times are
ticks.

### End

The k-th digital channel's `end`, as an i64 at offset 8k, for every digital channel in channel
order.

The recording ended. The end record follows the block that closed at the recording's end, and
nothing follows it. That block's end is the recording's length.

## Writing a journal

The recorder:

- Writes the preamble and the head before anything else. It may hold items until it knows what
  the head needs, such as an analog channel's `first`.
- Takes items in tick order across its channels. It closes a block once the block's decoded size
  reaches `block_bytes`, or once the block has been open for `time_limit_ms`. It never closes one
  between two items at the same tick.
- Writes a gap or a break before the block that needs it.
- Syncs the file to disk after each block, so the time limit bounds what a crash loses.
- At the end, closes the last block with the recording's end as its reason, then writes the end
  record.

## Converting a journal

The converter:

1. Reads the records up to the first bad one. It refuses a journal with no head or no block.
2. Takes the recording's length from the last block's end. A digital channel's `end` comes from
   the end record, or is the length when there is none. A gap whose `to` is not below the length
   is dropped, and the channel's `end` becomes the gap's `from`. A gap from tick 0 leaves an `end`
   of 0: the level is never known, and the channel's `initial` becomes false.
3. Sums each channel's counts over the blocks. It keeps every break whose `index` is below the
   channel's count.
4. Copies each block closed on its size as it is. Each run of blocks closed on the time limit,
   with the block closed at the end if it follows one, is decoded and cut again at `block_bytes`,
   the way an encoder cuts. The run's first block starts where the run starts. The converter
   compresses the new blocks at its own zstd level: the journal does not record one.
5. Computes the overview and the content id from every block's items. It decodes each block once.
6. Writes the recording. Each annotation source gets its generator file, `rec.<source>.jsonl`.
   When the recording has no hand-written sidecar yet, the converter writes one that includes
   them. An annotation that starts at or past the length is dropped, and a span that ends past it
   ends at the length.

A journal whose blocks all closed on their size, apart from the last, converts without
recompressing anything. A converted journal has the same content id as an offline conversion of
the same data, though its blocks can be cut differently.

## Reading a journal as it grows

A viewer can read a journal while the recorder writes it. A bad last record there means the
recorder has not finished writing it, so the viewer reads it again once the file grows. The
overview's levels have fixed shifts (12, 16, 20 and so on), so a viewer extends each level bucket
by bucket as blocks arrive, and adds a coarser level once the length needs one.
