# Zips

Captures shared zipped, as GitHub takes them attached to an issue or a discussion, each written the
way a tool people use writes it. `make.sh` writes them from the committed recordings and
`testdata/import/demo-logic.vcd`. `crates/import/tests/zipped.rs` reads them with the zip reader,
and `e2e/attachments.spec.ts` opens them on the Open page.

| File | What it holds, and how it is written |
| --- | --- |
| `stored.zip` | `identify-swd.tickwave` and a one-annotation sidecar, stored by Info-ZIP, whose local headers carry 4 more bytes of extra fields than the central directory says |
| `deflated.zip` | the same, deflated by Info-ZIP with no extra fields (`zip -X`) |
| `streamed.zip` | the same, as a streaming writer leaves it: each file's sizes after its data and in the central directory only (flag bit 3); the recording stored, the sidecar deflated |
| `macos.zip` | the same in a folder, deflated and streamed, with each file's resource fork under `__MACOSX/` as macOS's Compress adds them |
| `set.zip` | a folder of two recordings, `identify-swd.tickwave` with its sidecar and `identify-jtag.tickwave` |
| `capture.zip` | a folder holding `demo-logic.vcd`, to convert |
| `encrypted.zip` | `identify-swd.tickwave` encrypted: refused |
| `bzip2.zip` | `demo-logic.vcd` compressed with bzip2 by 7z: refused |
| `duplicate.zip` | `identify-swd.tickwave` in two folders: refused, since either could be the one meant |
| `nothing.zip` | a `README.txt` and nothing the page opens |
