# The converter

`tickwave` is a command-line program. It converts captures into tickwave recordings, describes a recording, opens one in your browser, checks one against the capture it came from, checks and bundles a recording's sidecar of annotations, finishes a recording that was cut off before its end, and re-cuts a recording that a recorder flushed often into blocks of the usual size. [The Open page](../open.html) runs the same conversion in a browser.

## Building it

You need a current Rust toolchain, a C compiler and a checkout of tickwave's repository. From the repository's root:

```sh
cargo build --release -p escapement-tickwave-cli
```

The program is then `target/release/tickwave`. `tickwave help`, and `--help` after any command, list its options.

To open recordings in your browser, the program carries the viewer. Build the web pages first, as the repository's README says, then:

```sh
cargo build --release -p escapement-tickwave-cli --features viewer
```

Without the viewer, `tickwave open` and `--open` refuse, and say how to build it.

## Converting a capture

`tickwave convert` reads three kinds of capture:

- `saleae`: Saleae Logic 2's binary export, version 0 or 1, a directory with one file a channel.
- `sigrok`: a sigrok session file (`.sr`), as PulseView, sigrok-cli and verge write them.
- `vcd`: a value change dump (`.vcd`). It takes one-bit signals and lists the rest as skipped.

The converter doesn't read Logic 2's own `.sal` files, because Saleae doesn't document that format and may change it. Export the capture from Logic 2 first: click Options at the bottom right, then Export Raw Data, and choose the binary format.

The converter writes the recording beside its destination first. Then it decodes what it wrote and compares it with the capture, edge by edge and sample by sample. Only a recording that matches takes its place, and if they differ, nothing is left behind.

Every time in a recording is a whole number of ticks from the capture's start, and one tick is the source's resolution. A Saleae export's times become ticks by `round((t − begin) × rate)`. The converter refuses an export with an edge more than 0.01 of a tick off that grid, rather than move it.

### From a Saleae export

Name each channel you want with `--channel`, as the export's file name without `.bin`, then the channel's name. Channels you don't name stay out.

```sh
tickwave convert saleae identify-swd/ --channel digital_3_0=SWCLK --channel digital_3_1=SWDIO \
    --title identify-swd -o identify-swd.tickwave
```

It reports the file it wrote, its size, its blocks and its content id:

```text
identify-swd.tickwave: 0.00 MB, 2 blocks, blake3:ccc8b7d8c3e4cb30da7fc6088e54ebeba2858ad402eb3fb5a9aeb50be989accf in 0.0 s
```

Logic 8, Logic Pro 8 and Logic Pro 16 write version 0 of the export, and its digital files don't record the sample rate. If you name an analog channel, the converter takes the rate its file records. Otherwise it infers the rate from the edges of every digital file in the export, named or not. It takes the lowest rate that divides 1 GHz and puts every edge on its grid, each on a tick of its own. With many edges, that is the capture's rate. With only a few, it can come out lower, and the recording then claims coarser timing than the capture had, never finer. An export with no edges has no rate to infer.

Give the rate with `--rate`, in hertz with an optional k, M or G, such as `--rate 50M`. The converter refuses a rate that puts an edge off its grid. It can't catch a rate higher than the capture's, though: the recording would then claim finer timing than the capture had. The recording says whether its rate was inferred or given.

### From a sigrok session

```sh
tickwave convert sigrok session.sr -o session.tickwave
```

Without `--channel`, every enabled channel goes in under its own name. With it, only the channels you name go in, as `--channel <probe name>=<name>`.

### From a VCD file

```sh
tickwave convert vcd dump.vcd -o dump.tickwave --channel top.dut.clk=CLK
```

Without `--channel`, every one-bit signal goes in, named by its scope path. With it, name each signal by its scope path.

### Analog channels

Where a capture's analog values are the instrument's integer codes, the converter stores the codes and the scale that turns them back into values. It checks that every sample comes back exactly first. Otherwise it stores the values as 32-bit floats, and `--no-codes` asks for floats anyway. A readout shows a coded value to the digit one code step carries.

If the source doesn't state a channel's unit, give it with `--unit <name>=<unit>`, such as `--unit A0=V`.

### Moving and trimming channels

`--offset <name>=<time>` moves a channel along the time axis, later when the time is positive: `--offset SWDIO=-40ns` moves SWDIO 40 ns earlier. It corrects a known delay in a channel's path, such as a longer probe lead. The time has to be a whole number of the capture's ticks.

`--keep <name>=<from>..<to>` keeps only that part of a channel, in the capture's time before any move: `--keep SWCLK=1.2ms..9.8ms`. Leave out either end to keep from the channel's start or to its end.

`<name>` is the name the channel takes in the recording, and a time is ticks or a decimal with a unit, as a sidecar writes it. The recording says how far each channel was moved and, for every channel, where it holds the capture's data, so neither a move nor a cut is taken for what the instrument recorded. A recording with a channel moved or trimmed starts where its earliest kept data does. An analog channel's cut moves forward to the next sample that keeps every sample on its tick.

### Options every conversion takes

| Option | Does |
|---|---|
| `-o`, `--output <file>` | where to write the recording |
| `--title <text>` | the recording's title, which a view shows; without one, a view shows the file's name |
| `--meta <key>=<value>` | free metadata, such as `--meta probe=verge`, as often as you like |
| `--offset <name>=<time>` | move a channel, later when the time is positive (above) |
| `--keep <name>=<from>..<to>` | keep only that part of a channel (above) |
| `--level <n>` | the zstd compression level, 19 by default |
| `--block-kib <n>` | how much decoded data each of a channel's blocks aims to hold, 1024 KiB by default and at most 65,536 |
| `--open` | open the recording in your browser once it's written, as [`tickwave open`](#opening-a-recording) does |

A block holds one channel's edges or samples over one stretch of time, and it is the smallest piece of a channel's data a viewer fetches. Smaller blocks mean less waiting for a narrow view of a long capture, and a larger file.

Every command that reads a recording takes `--max-decoded-mib <n>`: the most it decodes from one block, 64 MiB by default. A recording that needs more is refused, as a page's viewer refuses it unless the page sets a larger limit. A block at the largest `--block-kib` stays under the default.

## Converting in a browser

[The Open page](../open.html) converts a capture without installing anything. Choose a Saleae export's folder, or a `.sr` or `.vcd` file, or a `.zip` holding one, or drop it on the page. It converts at once, every channel under the capture's own name, and shows it in a view.

Under the view, the page lists the capture's channels, with the options a command line takes: the channels to take and their names, units, a title, metadata, the zstd level, the block size, `--no-codes`, and for a version 0 Saleae export, `--rate`. Leave the rate empty to infer it, as the command line does without `--rate`.

In the view, a grip at the left of each row's name moves its channel. Drag it sideways to move the channel in time, as `--offset` does, or up or down to change its row. Zoomed in far enough to show edges, a channel dragged to within a few pixels of another row's edge puts its own nearest edge on it; hold Alt to move it freely. The bars at the ends of a channel's data trim it, as `--keep` does, and the part left out fades. Trim every channel to the selection keeps the part of each channel you selected. With a grip or a bar focused, the arrow keys move it. The list shows each channel's move and trim, and takes them typed too, as times such as `-40 ns`. A move takes the channel's trim with it, so moving and trimming can come in either order. The view always shows all of the capture, so a trim can be undone.

Save keeps the recording. With nothing changed, that is the capture as converted. Otherwise Save converts the capture again with the list's settings first. The recording saved then opens, and its annotations can be edited. Shape again goes back to the capture, with the settings as they were.

For a Saleae export, it first checks that every edge sits on the sample grid, in order. Then it goes through the capture twice: once to look for exact integer codes in each analog channel, and once to encode.

The page runs the converter's own code, compiled to WebAssembly, so the same capture and settings give the same file, byte for byte. It stores channels in the order it lists them, which for a Saleae export is by file name, numbers in order, until you move one. A command line that names the same channels in that order, with the same moves and trims, makes the same file.

Under the view, Add a capture or recording takes another to merge with the one shown, as [`tickwave merge`](#merging-recordings) does. Drop it on the view, or choose it: a `.tickwave` recording with its sidecar's files, a capture, which converts whole first, or a `.zip` of either. The view then shows them all on one time axis, each recording's rows under its name. Drag the grip by a recording's name sideways to move the whole recording in time, as `--align` does, and line it up with the others. An edge of it lands on another recording's edge within a few pixels; hold Alt to move it freely. The list gives each recording a head with its alignment, which takes a time typed too, and Remove. A channel's own grip and bars work as for one capture. Save merges the recordings with the list's settings and opens the result.

The capture stays in the browser. The page reads a Saleae export a slice at a time, so the export can be larger than the 4 GiB WebAssembly gives the converter. A sigrok or VCD file is read whole, and the recording is built in memory, so these have to fit. The page converts on one thread, where the command line uses every core, so a large capture takes longer.

## Merging recordings

```sh
tickwave merge analyser.tickwave probe.tickwave -o both.tickwave
```

This puts several recordings on one time axis, such as a logic analyser's beside a probe's own stream from the same target. A capture goes through `tickwave convert` first. Every channel of every recording goes in, in order, under its own name. A channel whose name an earlier recording took is named after its file as well: `probe/CLK`.

The merged recording ticks at the least common multiple of the recordings' tick rates: 500 MHz and 96 MHz merge at 12 GHz, a tick of 83 ps. Where that rate would need a number past 2^53, it ticks at the fastest recording's rate instead. Each channel keeps its own instrument's ticks, and the merged recording states where each instrument's ticks lie on its time axis. Nothing is resampled: every edge and sample keeps its time. A reading still keeps to the tick of the recording it reads, so it never shows more digits than that recording carries.

The recordings share no clock: none of the captures tickwave reads records the time of day it started. So each recording starts at the merged recording's start. `--align <n>=<time>` moves recording `n`, counting from 1, later when the time is positive, to line it up with the others, on an edge both instruments saw, say. The time is a decimal with a unit, or a whole number of the merged recording's ticks. Those ticks are at the merged rate the merge picks, so a time with a unit says the same thing whichever rate that is. The merged recording states an alignment as a claim, as it does a move.

Instruments' clocks drift apart. At 20 ppm, an alignment by one edge stays within one tick of a 96 MHz clock for about half a millisecond either side of the edge, and within 10 µs for about half a second. `--stretch <n>=<ratio>` lengthens recording `n`'s ticks to make up for it, as a ratio such as `1001/1000` or a change such as `+20ppm`. A stretch above 1 says the recording's clock ran slow. The merged recording states the stretch on the recording's sources, as a claim. What a stretch leaves is the clocks' wander.

Two marks give both at once: `--align <n>=<x>@<y>,<x>@<y>`. Each mark is an event both instruments saw, `x` its time in recording `n`'s own time and `y` its time on the merged recording's axis, both with a unit. For a recording left unaligned, the axis time is that recording's own time. The first mark gives the alignment, and the time between the two gives the stretch. `--align 2=1s@1.0016s,9s@9.0024s` puts the probe's own 1 s at 1.0016 s and its 9 s at 9.0024 s: a stretch of 1.0001, and its tick 0 1.5 ms in. The stretch is out by each mark's error over the time between them, so take the marks far apart. The merge takes the simplest ratio within that error, with each `x` good to one tick of recording `n`'s coarsest instrument and each `y` to one merged tick.

A mark has to be an event the instruments can't confuse with another. A periodic signal, such as a 100 Hz marker, lines up equally well at every whole number of its periods: an alignment on one of its edges can be off by a period, and nothing in the recording says so. Align on an aperiodic mark, such as a burst, a reset or a single pulse, and use the periodic signal to measure the drift.

| Option | Does |
|---|---|
| `--channel <n>:<name>[=<new>]` | take channel `<name>` of recording `n`, named `<new>` if given. A recording with no `--channel` gives every channel |
| `--align <n>=<time>` | move recording `n`, later when the time is positive |
| `--align <n>=<x>@<y>,<x>@<y>` | place recording `n` by two marks, which give its alignment and its stretch |
| `--stretch <n>=<ratio>` | lengthen recording `n`'s ticks by a ratio `a/b` or a change such as `+20ppm` |
| `--offset <name>=<time>` | move a channel, later when the time is positive, by a whole number of its own instrument's ticks |
| `--keep <name>=<from>..<to>` | keep only that part of a channel, in its own recording's time before any move |
| `--unit <name>=<unit>` | an analog channel's unit |

`<name>` is the name the channel takes in the merged recording. `-o`, `--title`, `--meta`, `--level`, `--block-kib` and `--open` work as they do for a conversion. Each recording's own title and metadata stay with its sources, where `tickwave info` and the Open page's details list them.

A recording's generated annotations come with it, from its decoders' files, bundled or in a sidecar beside it. Their times move with the recording, and [ANNOTATIONS.md](https://github.com/Escapement-Project/tickwave/blob/main/ANNOTATIONS.md#merging) says how their names follow the merge's. Annotations written by hand don't come, since their times are the recording's own, and `merge` says how many it left out. A derived channel, such as power from a current and a voltage, comes while both its channels do, moved and trimmed alike.

A channel's move is in its own instrument's ticks, since that is where its edges and samples sit: a whole number of them, or a time with a unit within a hundredth of one.

[`tickwave verify`](#checking-a-recording-against-its-capture) checks a merged recording against the recordings it merged. It goes by what the merged recording states, not by the options it was given.

## Describing a recording

```sh
tickwave info identify-swd.tickwave
```

```text
0.523583100 s at 1000000000/1 ticks a second, 2 channels, blake3:ccc8b7d8c3e4cb30da7fc6088e54ebeba2858ad402eb3fb5a9aeb50be989accf
  source 0: saleae-binary-1 at 1000000000/1 ticks a second, its tick 0 at 0: digital_3_0.bin, digital_3_1.bin
  SWCLK        digital, 15286 edges, starts 1, known to 0.523583070 s (digital_3_0)
  SWDIO        digital, 1874 edges, starts 1, known to 0.523583100 s (digital_3_1)
0.005 MB: footer 846 bytes, 2 blocks of 1424-1727 bytes, median 1727
  SWCLK        overview strip at 2^21
  SWDIO        overview strip at 2^24
```

Times are in seconds to one tick, here a nanosecond: never more digits than the recording carries. Each channel has an overview of its own: levels of buckets of 2^n ticks, which a view draws from when it is zoomed out, from finer ones in the file to the strip in the footer. `--json` prints the header and the block statistics as JSON, for a script. `info` reads the recording's footer, at its end, and its index a page at a time, so it holds little however large the recording.

For a merged recording, `info` lists each instrument's source, with how many of the merged recording's ticks one of its ticks lasts and any stretch. Where two instruments may have recorded the same periodic signal, such as a marker line, it says that `--drift` measures the drift between their clocks. `tickwave info --drift` decodes the whole recording for it. Here an analyser at 1 GHz and a probe at 100 MHz both recorded a 10 ms marker, and the probe's clock ran 1,000 ppm fast, so it measured each period as 10.01 ms:

```sh
tickwave info --drift both.tickwave
```

```text
9.029020000 s at 1000000000/1 ticks a second, 2 channels, blake3:635af47f259cfd10d5f9e750e2f82d98a894be04206c7031692a5836b72b1137
  source 0: vcd at 1000000000/1 ticks a second, its tick 0 at 0: analyser.vcd
  source 1: vcd at 100000000/1 ticks a second (10 of the recording's ticks each), its tick 0 at 0: probe.vcd
  analyser.MARK digital, 1800 edges, starts 0, known to 9.020000000 s (analyser.MARK, source 0)
  probe.MARK   digital, 1800 edges, starts 0, known to 9.029020000 s (probe.MARK, source 1)
0.002 MB: footer 1009 bytes, 2 blocks of 46-76 bytes, median 76
  analyser.MARK overview strip at 2^28
  probe.MARK   overview strip at 2^28
drift: analyser.MARK (source 0) against probe.MARK (source 1): +1000.0 ± 0.0 ppm, the stretch source 0 would take to tick as source 1 does; periods 10.000000 ms and 10.010000 ms over 900 and 900 rises, 0.6 µs and 1.7 µs rms off a straight line
```

It fits each such channel's rising edges to a line. A missed pulse counts as a period with no rise in it, not as one period twice as long. It compares two channels from different instruments whose periods agree within 1 %, and reads only channels of at most 2^20 edges. Their periods' ratio is the drift, as the stretch that would make the first instrument tick as the second does. A recording already stretched shows what the stretch leaves. The rms is each channel's wander about its line.

## Opening a recording

```sh
tickwave open identify-swd.tickwave
```

```text
identify-swd.tickwave: with its sidecar identify-swd.annotations.toml, 695 annotations
http://127.0.0.1:38343/open.html?src=bc6452795da2010a0ba21af9322bc366/identify-swd.tickwave&annotations=bc6452795da2010a0ba21af9322bc366/identify-swd.annotations.toml&save=bc6452795da2010a0ba21af9322bc366/identify-swd.annotations.toml&local
serving the viewer until Ctrl-C
```

This shows the recording in your browser, on [the Open page](../open.html), with its sidecar's annotations. A page can't read a file from your disk, so `tickwave open` serves the recording and the viewer itself. It serves them to your own computer only, at 127.0.0.1, and from your disk it serves only the recording, its sidecar and the files the sidecar includes. The viewer reads the recording as you move through it, so it keeps serving until you stop it with Ctrl-C.

It takes the sidecar beside the recording by its name: `identify-swd.annotations.toml` for `identify-swd.tickwave`. `--annotations <file>` names another. It checks the sidecar first and lists any problems, and the viewer then shows the recording without its annotations. A recording cut off before its end is refused, and `tickwave repair` finishes it.

It prints the viewer's address and starts your browser on it. `--no-browser` only prints the address, for a browser you start yourself. `--port <n>` serves on that port, where otherwise the system chooses one.

The page can [edit the sidecar's annotations](viewer.html#editing-annotations) and save the file back beside the recording. A recording with no sidecar gets one on the first save: `identify-swd.annotations.toml` for `identify-swd.tickwave`. The page won't save over a sidecar that changed on disk after the page read it, or over one with problems.

A conversion opens its recording the same way with `--open`, once the recording is written. `--port` and `--no-browser` go with it:

```sh
tickwave convert sigrok capture.sr -o capture.tickwave --open
```

## Checking a recording against its capture

```sh
tickwave verify identify-swd.tickwave --against identify-swd/
```

It decodes the recording and compares it with the capture again, edge by edge and sample by sample, and says whether they match:

```text
identify-swd.tickwave: matches identify-swd/
```

For a Saleae export, `--against` takes its directory. A recording from a version 0 export is checked at the rate it was made at.

A recording with a channel moved or trimmed is checked through its moves and cuts, as the recording states them.

A merged recording takes `--against` once for each recording merged into it, in the merge's order. Each is the recording's capture, or the `.tickwave` recording itself where there is no capture to import, such as a probe's own stream:

```sh
tickwave verify both.tickwave --against analyser/ --against probe.tickwave
```

It first checks that each one's sources are the ones the merged recording names in that place, and says so if two are swapped. Once the recording matches, it reports the drift between its instruments, as `info --drift` does.

## Checking a sidecar

A recording's annotations live beside it, in a sidecar: one hand-written TOML file, and the JSON Lines files of any programs that generated annotations. [The sidecar](annotations.html) describes them. `tickwave annotations check` checks a sidecar against its recording: its ids, its refs, its included files, its channels, its times and the recording id it names.

```sh
tickwave annotations check identify-swd.tickwave identify-swd.annotations.toml
```

```text
identify-swd.annotations.toml: 695 annotations, 8 hand-written and 687 generated, all check
```

It reads the included files from beside the TOML file, and refuses an included name with a directory in it. If it finds a problem, it lists every one and exits with a non-zero status, so a page's build stops instead of publishing a broken reference:

```text
identify-swd.annotations.toml: annotation 1: ref "swd/wait/1" does not resolve
Error: 1 problem
```

`--json` prints the annotations as resolved: times in ticks, refs followed, in the sidecar's order.

## Bundling a sidecar into its recording

```sh
tickwave bundle identify-swd.tickwave identify-swd.annotations.toml -o identify-swd.bundle.tickwave
```

This writes a copy of the recording with the sidecar's files inside, so one file carries both. It checks the sidecar first. The copy's content id is the recording's: a bundle holds the same capture. A view of a bundled recording shows its annotations when the page names no sidecar.

## Repairing a recording

A recorder writes a recording as it captures, and the file ends with an index of what it holds. A crash, a power cut or a full disk can stop the file before its index, and a viewer then can't open it. `tickwave repair` finishes such a file:

```sh
tickwave repair identify-swd.cut.tickwave -o identify-swd.tickwave
```

```text
identify-swd.tickwave: 2952 of 3000 bytes kept, 8 blocks to 0.388970979 s, blake3:0ba2d62a24ad16ad2f44142d329ed63750eb97e66629611c9c233ff08f4d7b1a
```

A recorder that flushes notes, after each flush, the time up to which every channel's data is in the file. Where the file holds such a note, repair keeps every channel to the latest one. Otherwise it keeps each channel's blocks up to its last whole one, and the capture ends where the latest of those ends. Either way what came after was lost with the rest of the file. It then writes the index and the footer, and writes the copy to `-o`. If the recorder generated annotations, each generator's file goes beside the copy, as `<name>.<generator>.jsonl`. A finished recording is refused, since there is nothing to repair. So is one with a channel moved or trimmed: convert it again from its capture.

## Re-cutting a recording

A recorder that flushes often, as a live capture does so that a crash loses little, closes each channel's block at every flush. At a low data rate those blocks are small, and each costs the same few dozen bytes of framing and index, so the file comes out larger than its data. `tickwave recut` writes it again with blocks of the usual size. The repository's `web/fixtures/energy.tickwave` is laid out this way: a synthetic recording of five channels over 30 ms, its current and voltage in six short blocks each, as a recorder flushing every 5 ms leaves them:

```sh
tickwave recut energy.tickwave -o energy.recut.tickwave
```

```text
energy.recut.tickwave: 0.01 MB from 0.02 MB, 5 blocks, blake3:ed46435e115fa7f0cf831415a7cb8452cbf413a496cba9c974abb8b115e8a21a in 0.0 s
```

`tickwave info` counts 23 blocks of 13 to 360 bytes in the original and 5 of 21 to 530 bytes in the copy, 18 KB against 15 KB.

The copy holds the same content, with the same content id, and is checked before it is written. `--block-kib` and `--level` work as they do for a conversion. A recording cut off before its end is finished as `repair` would finish it, and each annotation generator's file goes beside the copy.
