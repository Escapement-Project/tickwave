# escapement-tickwave-decoders

tickwave's reference protocol decoders, written against `escapement-tickwave-decode`'s contract
alone, as a decoder loaded at run time would be.

| Decoder | Reads | Decodes |
|---|---|---|
| `swd` | SWCLK and SWDIO | Serial Wire Debug transfers: request, acknowledge, data and parity, and their bits |
| `jtag` | TCK, TMS, TDI and TDO, or cJTAG's output | the TAP controller's states and paths, and the IR and DR scans with their TDI and TDO bits; with `chain` set, each TAP's part of a scan |
| `cjtag` | TCKC and TMSC | IEEE 1149.7: escapes, control levels and commands, Check Packets, scan formats and the OScan1, 2, 5 and 6 Scan Packets, and the clocks JTAG reads from them |
| `jtag-dp` | JTAG's TAP parts | Arm's JTAG Debug Port: its instruction, and each DPACC and APACC access as a transfer |
| `adiv5` | SWD's or JTAG-DP's transfers | ADIv5 DP and AP register accesses, each AP's registers named by the class its IDR gives |
| `mem-ap` | ADIv5's accesses | MEM-AP memory accesses: the address, size and bytes each moved, single, packed, wide or banked |

So the stacks are SWD, ADIv5 and MEM-AP; JTAG, JTAG-DP, ADIv5 and MEM-AP; and cJTAG with the same
stack on its JTAG. Each module's doc says what its decoder follows and what it leaves unknown,
with the reason.

## Settings

Each setting gives what a decoder can't learn from a recording that starts partway through a
session, or a choice the wire leaves open:

- `swd`: `turnaround`, one to four cycles, and `orundetect`, as they stand where the recording
  starts. Writes to DLCR and CTRL/STAT on the wire change both from there.
- `jtag`: `chain`, each TAP's IR length, the TAP nearest TDO first, to cut scans into each TAP's
  part. `tdo_edge`, the TCK edge TDO is read at: the rise, the fall after it, or whichever the IR
  scans' captures show, the default.
- `jtag-dp`: `tap`, the debug port's place in JTAG's chain.
- `adiv5`: `idr`, each AP's IDR where the recording starts, until an IDR read gives it.
- `cjtag`: `start_up`, whether a reset leaves the controller online or offline, and
  `assume_qualified`, whether the target drives TDO where the controller's counts aren't known.

## Recipes

With the `recipe` feature, `recipe::registry()` holds these decoders for a host binding a recipe
(`escapement-tickwave-decode`'s README describes recipes), and offers three stacks whole: the
recipes `swd.toml`, `jtag-adi.toml` and `cjtag-adi.toml` in `recipes/`. `jtag.toml` and
`cjtag.toml` there are JTAG alone and cJTAG with JTAG, for a chain the presets don't fit.

The JTAG presets take one Arm debug TAP with a 4-bit IR. A chain with more TAPs needs its own
`chain`, and JTAG-DP's `tap` the debug TAP's place in it.

## Checking against a recording

The examples run a stack over a recording and print what it decoded, to compare with another
implementation or check against what a standard guarantees:

```sh
cargo run --release -p escapement-tickwave-decoders --example swd_canon -- capture.tickwave
cargo run --release -p escapement-tickwave-decoders --example jtag_check -- capture.tickwave
cargo run --release -p escapement-tickwave-decoders --example cjtag_check -- capture.tickwave
```

- `swd_canon` and `jtag_canon` print counts and a digest of every item rendered by name, which
  doesn't depend on how operations are encoded.
- `jtag_check` checks JTAG where TDO is recorded against what IEEE 1149.1 guarantees: every IR
  scan captures a value whose two lowest bits are 01, and a DR scan after Test-Logic-Reset, before
  any IR scan, shifts out IDCODE or BYPASS.
- `cjtag_check` checks the JTAG that cJTAG feeds in the same way, and prints cJTAG's counts.
- `step_times`, `view_times`, `cjtag_seek` and `cjtag_window` time the host: a step, a decode
  view's forward pass on a worker, a seek through checkpoints, and a window decoded beside the
  pass.

The tests run every stack on seeded random wires and through the host, and `fuzz/` has a target
for each stack and one for the host.
