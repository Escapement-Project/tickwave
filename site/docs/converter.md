# The converter

`chrono` is a command-line program. It converts captures into chrono recordings, describes a recording, checks one against the capture it came from, and checks and bundles a recording's sidecar of annotations.

## Building it

You need a current Rust toolchain and a checkout of chrono's repository. From the repository's root:

```sh
cargo build --release -p chrono-cli
```

The program is then `target/release/chrono`. `chrono help`, and `--help` after any command, list its options.

## Converting a capture

`chrono convert` reads three kinds of capture:

- `saleae`: Saleae Logic 2's binary export, a directory with one file a channel.
- `sigrok`: a sigrok session file (`.sr`), as PulseView, sigrok-cli and verge write them.
- `vcd`: a value change dump (`.vcd`). It takes one-bit signals and lists the rest as skipped.

Before it writes a recording, the converter decodes what it made and compares it with the capture, edge by edge and sample by sample. It writes nothing if they differ.

Every time in a recording is a whole number of ticks from the capture's start, and one tick is the source's resolution. A Saleae export's times become ticks by `round((t − begin) × rate)`. The converter refuses an export with an edge more than 0.01 of a tick off that grid, rather than move it.

### From a Saleae export

Name each channel you want with `--channel`, as the export's file name without `.bin`, then the channel's name. Channels you don't name stay out.

```sh
chrono convert saleae identify-swd/ --channel digital_3_0=SWCLK --channel digital_3_1=SWDIO \
    --title identify-swd -o identify-swd.chrono
```

It reports the file it wrote, its size, its blocks and its content id:

```text
identify-swd.chrono: 0.00 MB, 1 blocks, blake3:930db20d55ecede1084b73ebbdc7edd306268b900746c08abd0bdfdb0f121623 in 0.0 s
```

### From a sigrok session

```sh
chrono convert sigrok session.sr -o session.chrono
```

Without `--channel`, every enabled channel goes in under its own name. With it, only the channels you name go in, as `--channel <probe name>=<name>`.

### From a VCD file

```sh
chrono convert vcd dump.vcd -o dump.chrono --channel top.dut.clk=CLK
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
| `--block-kib <n>` | how much decoded data each block aims to hold, 1024 KiB by default |

A block is the smallest piece of data a viewer fetches. Smaller blocks mean less waiting for a narrow view of a long capture, and a larger file.

## Describing a recording

```sh
chrono info identify-swd.chrono
```

```text
identify-swd
0.523583 s at 1000000000/1 ticks a second, 2 channels, blake3:930db20d55ecede1084b73ebbdc7edd306268b900746c08abd0bdfdb0f121623
  SWCLK        digital, 15286 edges, starts 1, known to 0.523583 s (digital_3_0)
  SWDIO        digital, 1874 edges, starts 1, known to 0.523583 s (digital_3_1)
0.005 MB: head 1765 bytes (3 overview levels), 1 blocks of 3086-3086 bytes, median 3086
```

`--json` prints the header and the block statistics as JSON, for a script.

## Checking a recording against its capture

```sh
chrono verify identify-swd.chrono --against identify-swd/
```

It decodes the recording and compares it with the capture again, edge by edge and sample by sample, and says whether they match:

```text
identify-swd.chrono: matches identify-swd/
```

For a Saleae export, `--against` takes its directory.

## Checking a sidecar

A recording's annotations live beside it, in a sidecar: one hand-written TOML file, and the JSON Lines files of any programs that generated annotations. [The sidecar](annotations.html) describes them. `chrono annotations check` checks a sidecar against its recording: its ids, its refs, its included files, its channels, its times and the recording id it names.

```sh
chrono annotations check identify-swd.chrono identify-swd.annotations.toml
```

```text
identify-swd.annotations.toml: 695 annotations, 8 hand-written and 687 generated, all check
```

It reads the included files from beside the TOML file. If it finds a problem, it lists every one and exits with a non-zero status, so a page's build stops instead of publishing a broken reference:

```text
identify-swd.annotations.toml: annotation 1: ref "swd/wait/1" does not resolve
Error: 1 problem
```

`--json` prints the annotations as resolved: times in ticks, refs followed, in the sidecar's order.

## Bundling a sidecar into its recording

```sh
chrono bundle identify-swd.chrono identify-swd.annotations.toml -o identify-swd.bundle.chrono
```

This writes a copy of the recording with the sidecar's files inside, so one file carries both. It checks the sidecar first. The copy's content id is the recording's: a bundle holds the same capture. A view of a bundled recording shows its annotations when the page names no sidecar.
