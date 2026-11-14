# tickwave sidecar annotations, version 1

Annotations mark spans and points on a recording: a transfer, an acknowledge, a misread, a
stretched clock period. They live beside the recording in text files, so they diff, take hand
edits, and regenerate without touching the capture. Page text cites an annotation by its id.

Status: draft, with the recording format.

## Files

A recording `rec.tickwave` has one hand-written TOML file, `rec.annotations.toml`. A program that
generates annotations, such as a protocol decoder, writes its own JSON Lines file, which the TOML
file includes: `rec.wire_swd.jsonl`. An included file sits beside the hand-written one and is named
on its own, without a directory. A generator rewrites its file whole on each run. It may start
the hand-written file when there is none, and never writes it after that. From then on only
tickwave's editor edits it, and it keeps the file's comments, order and layout, so a rerun cannot
touch a hand edit. A script that re-pins a sidecar to a reconverted recording may rewrite
`recording_id`, and nothing else.

A generator runs after the recording is converted. It reads the recording's id from
`tickwave info --json` (`header.content_id`).

## The hand-written file

```toml
format = "tickwave-annotations/1"
recording = "rec.tickwave"               # optional; checked against the file name when known
recording_id = "blake3:4f2a…"            # the recording's content id
include = ["rec.wire_swd.jsonl"]         # generator files, beside this one

[[annotation]]
id = "read-ack-check"
start = "317.85882 ms"
end = "317.85891 ms"
channels = ["SWCLK", "SWDIO"]
label = "The 12-cycle period before read data"
kind = "period"
measure = { channel = "SWCLK", what = "rate" }

[[annotation]]
id = "first-misread"
ref = "wire_swd/misread/412093311"
label = "NXP's firmware misreads at 25 MHz"
```

## A generator's file

The first line names the generator. Each later line is one annotation. Blank lines are skipped.

```
{"format": "tickwave-annotations/1", "source": "wire_swd", "version": "9f3c2e1", "recording_id": "blake3:4f2a…"}
{"id": "misread/412093311", "kind": "misread", "start": 412093311, "end": 412094105, "channels": ["SWDIO"], "label": "parity wrong"}
```

`source` is the generator's name. It is not empty and holds no whitespace, `/` or `\`: tickwave
names the generator's file after it, `rec.<source>.jsonl`, beside the recording. `version` is
optional: the generator's own commit or hash, for provenance.

## Fields

| Field | Meaning |
| --- | --- |
| `id` | Hand-written: any name without whitespace or `/`. Generated: the full id is `<source>/<id>`. |
| `start`, `end` | Times. `end` is absent for a point. |
| `channels` | Channel names. Absent means every channel. |
| `label` | Short text shown with the annotation. |
| `kind` | A free word (`transfer`, `misread`, `stall`). Styling and filtering key on it. |
| `text` | Longer text, shown in an inset's caption. |
| `paint` | A role (`probe`, `target`): the annotation's channels draw over its span in the colour the host page gives that role. The target's lines are also heavier, so the roles differ without colour. |
| `measure` | `{channel, what}`: the inset shows a measure taken from the channel's edges inside the span (below). The channel must be digital, and the annotation a span. |
| `values` | Figures the annotation states, by name, each a decimal and its unit (below). |
| `ref` | Hand-written only: the full id of a generated annotation. This one takes its span and channels, and any field it leaves out. |

A file may hold no other fields, `measure`'s included: a misspelt one is an error, not ignored.

## Measures

`measure` takes the channel's edges at ticks in `[start, end)`: an edge at `end` is outside. It
shows no more digits than its ticks carry. A span meant to measure from one edge to another starts
at the first edge's tick and ends one tick past the last.

- `rate` and `period`: the running clock. Its periods run from one rising edge to the next, so a
  span needs at least two rising edges. Sort the span's n periods as p[0..n). Take
  Q1 = p[⌊(n − 1)/4⌋], Q3 = p[⌈3(n − 1)/4⌉], and IQR = Q3 − Q1, but at least one tick. Keep the
  periods within [Q1 − 1.5 IQR, Q3 + 1.5 IQR]: Tukey's fences. The measure is the mean of the
  kept periods, as a rate or a period, and the inset says how many it kept ("mean of 31 of 32
  periods"). The fences keep a clock's edge jitter and its quantisation, such as 26 and 27 ns for
  a 26.67 ns clock. They drop pauses, and a long period that occurs in fewer than a quarter of
  the periods, such as a slow acknowledge check or a stretched cycle.
- `duration`: the time from the channel's first edge in the span to its last, of either
  polarity. A span needs at least two edges.

## Values

`values` holds the figures an annotation states, for a page's charts and tables to read. A decoder
that measures a session writes them beside its label:

```toml
[[annotation]]
id = "session-50"
ref = "wire_swd/session/377858886"
values = { asked = "50 MHz", read_data = "37.5 MHz", write_data = "49.9 MHz" }
```

In a generator's file the same is `"values": {"asked": "50 MHz", ...}`. A name is lower-case
letters, digits and `_`, starting with a letter. A value is a decimal, then one space and a unit
where it has one: a count, such as a marker's number, has none. It is a string, so `"81.0 KiB/s"`
keeps its last digit. tickwave never converts or rounds a value. A page compares units as text, so
micro is always `µ` (U+00B5 MICRO SIGN), as in times, and never the Greek letter mu (U+03BC).

## Times

A time is either a whole number of ticks from the capture's start, or a decimal with a unit: `s`,
`ms`, `us` (or `µs`), `ns`, `ps`. A decimal converts to ticks exactly, without floating point, and
rounds to the nearest tick, halves away from zero.

## Ids stay stable

Page text cites ids, so they must survive a decoder's rerun. A generator derives each id from the
recording, not from the run: its kind and start tick, as `misread/412093311`, not a counter. A
hand-written `ref` gives a generated annotation a name that reads well in prose.

## Order

Annotations keep sidecar order: the hand-written file's, in its order, then each included file's,
in the order `include` lists the files, line by line. Order carries meaning: a view steps through
the kind of the first annotation unless it was opened at another, so an author lists first an
annotation of the kind a page steps through.

## Resolved annotations

`tickwave annotations check --json` prints the annotations resolved, in sidecar order, and the
browser's decoder hands the page the same. Each has its full id, `source` (the generator's name;
null for a hand-written one), `start` and `end` in ticks, and the fields above. An alias keeps
`ref`, the full id of the generated annotation it names, so a page can tell the two apart.

## Checks

`tickwave annotations check rec.tickwave rec.annotations.toml` reports every problem it finds, then
exits non-zero if there was any. A page's build runs it, so a decoder rerun that drops an annotation
the prose cites breaks the build instead of the page. It checks that:

- both files declare format `tickwave-annotations/1`, and their `recording_id` matches the
  recording;
- `recording`, if given, names the recording's file;
- every included file is named without a directory, is provided, and every provided file is
  included;
- each generator's name is one the rule above allows;
- ids are unique, hand-written ids hold no `/`, and every `ref` names a generated annotation;
- every channel exists, a point and a span's `start` lie in `0..length`, and a span's `end` is
  not before its `start` and not past `length`;
- every `measure` names a digital channel, a known measure, and sits on a span;
- every value is a decimal, with one space and a unit where it has one, under a name of lower-case
  letters, digits and `_`.

## Bundling

`tickwave bundle rec.tickwave rec.annotations.toml -o shared.tickwave` checks the sidecar, then
writes a copy of the recording with the sidecar's files in its annotation bundle (each file's name
and text; FORMAT.md, "Annotations"), for a single-file share. The content id stays the same.

## Merging

`tickwave merge` carries each input's generated annotations into the merged recording: the generator
files its annotation bundle holds, or, where it has none, those its sidecar beside it includes.
They are bundled again under the merged recording's id. Each time moves with its input as its
input's sources do (FORMAT.md, "Sources"): a time `x` of the input's lies at the input's move plus
`floor(x × s × R / R_input)` on the merged recording's axis, `s` the input's stretch and
`R / R_input` the merged rate over the input's, and is written in its ticks. Inputs go in the order
given. A generator whose name an earlier input's generator took is renamed `<stem>-<name>`, where
`<stem>` is the input's file name without its extension, with any whitespace written as `_`. A
second clash takes `<stem>-2-<name>`, and so on. The full ids of its annotations change with it, and
so does the `source` its file's first line names, so a tool that looks for its own generator's file
by name finds only the first input's.

An annotation whose times move outside the merged recording is left out: a span that ends at or
before its start, or anything that starts at or past its end. A span that reaches over either end
is cut there. A line that doesn't read is left out too, and so is one that breaks a rule above for a
generator's line. The merge counts each of these.

Channel names in `channels` and `measure` follow the names the merge gives. A channel the merge
leaves out is dropped from `channels`, and an annotation left with none is left out. A `measure`
on a channel left out is dropped. An annotation without `channels`, which meant every channel of
its recording, names that input's channels.

A hand-written file's annotations are not carried, since their times are the input's. The merge
says how many it left out.
