# The converter

`tickwave` is a command-line program. It converts captures into tickwave recordings, describes a recording, checks one against the capture it came from, and checks and bundles a recording's sidecar of annotations. [The Open page](../open.html) runs the same conversion in a browser.

## Building it

You need a current Rust toolchain and a checkout of tickwave's repository. From the repository's root:

```sh
cargo build --release -p tickwave-cli
```

The program is then `target/release/tickwave`. `tickwave help`, and `--help` after any command, list its options.

## Converting a capture

`tickwave convert` reads three kinds of capture:

- `saleae`: Saleae Logic 2's binary export, a directory with one file a channel.
- `sigrok`: a sigrok session file (`.sr`), as PulseView, sigrok-cli and verge write them.
- `vcd`: a value change dump (`.vcd`). It takes one-bit signals and lists the rest as skipped.

Before it writes a recording, the converter decodes what it made and compares it with the capture, edge by edge and sample by sample. It writes nothing if they differ.

Every time in a recording is a whole number of ticks from the capture's start, and one tick is the source's resolution. A Saleae export's times become ticks by `round((t − begin) × rate)`. The converter refuses an export with an edge more than 0.01 of a tick off that grid, rather than move it.

### From a Saleae export

Name each channel you want with `--channel`, as the export's file name without `.bin`, then the channel's name. Channels you don't name stay out.

```sh
tickwave convert saleae identify-swd/ --channel digital_3_0=SWCLK --channel digital_3_1=SWDIO \
    --title identify-swd -o identify-swd.tickwave
```

It reports the file it wrote, its size, its blocks and its content id:

```text
identify-swd.tickwave: 0.00 MB, 1 blocks, blake3:930db20d55ecede1084b73ebbdc7edd306268b900746c08abd0bdfdb0f121623 in 0.0 s
```

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

### Options every conversion takes

| Option | Does |
|---|---|
| `-o`, `--output <file>` | where to write the recording |
| `--title <text>` | the recording's title, which a view shows; without one, a view shows the file's name |
| `--meta <key>=<value>` | free metadata, such as `--meta probe=verge`, as often as you like |
| `--level <n>` | the zstd compression level, 19 by default |
| `--block-kib <n>` | how much decoded data each block aims to hold, 1024 KiB by default and at most 65,536 |

A block is the smallest piece of data a viewer fetches. Smaller blocks mean less waiting for a narrow view of a long capture, and a larger file.

Every command that reads a recording takes `--max-decoded-mib <n>`: the most it decodes from one block, 64 MiB by default. A recording that needs more is refused, as a page's viewer refuses it unless the page sets a larger limit. A block at the largest `--block-kib` stays under the default.

## Converting in a browser

[The Open page](../open.html) converts a capture without installing anything, then opens it. Choose a Saleae export's folder, or a `.sr` or `.vcd` file, or drop it on the page. The page lists the capture's channels: choose the ones to take, and name them. It takes the options a command line does: units, a title, metadata, the zstd level, the block size and `--no-codes`. Then it converts, checks the recording against the capture, offers it to save, and shows it in a view.

For a Saleae export, it first checks that every edge sits on the sample grid, in order. Then it goes through the capture three times: once to look for exact integer codes in each analog channel, once to encode, and once to check the recording against the capture, edge by edge and sample by sample. A recording that fails the check is never offered to save.

The page runs the converter's own code, compiled to WebAssembly, so the same capture and settings give the same file, byte for byte. It stores channels in the order it lists them, which for a Saleae export is by file name, numbers in order. A command line that names the same channels in that order makes the same file.

The capture stays in the browser. The page reads a Saleae export a slice at a time, so the export can be larger than the 4 GiB WebAssembly gives the converter. A sigrok or VCD file is read whole, and the recording is built in memory, so these have to fit. The page converts on one thread, where the command line uses every core, so a large capture takes longer.

## Describing a recording

```sh
tickwave info identify-swd.tickwave
```

```text
identify-swd
0.523583100 s at 1000000000/1 ticks a second, 2 channels, blake3:930db20d55ecede1084b73ebbdc7edd306268b900746c08abd0bdfdb0f121623
  SWCLK        digital, 15286 edges, starts 1, known to 0.523583070 s (digital_3_0)
  SWDIO        digital, 1874 edges, starts 1, known to 0.523583100 s (digital_3_1)
0.005 MB: head 1767 bytes (3 overview levels), 1 blocks of 3086-3086 bytes, median 3086
```

Times are in seconds to one tick, here a nanosecond: never more digits than the recording carries. `--json` prints the header and the block statistics as JSON, for a script.

## Checking a recording against its capture

```sh
tickwave verify identify-swd.tickwave --against identify-swd/
```

It decodes the recording and compares it with the capture again, edge by edge and sample by sample, and says whether they match:

```text
identify-swd.tickwave: matches identify-swd/
```

For a Saleae export, `--against` takes its directory.

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
