# Repair's crash cases

Each file here is a cut-off recording on which `repair` once failed: the file the fuzz builder
(`fuzz/src/lib.rs`, `file`) makes of a crash input of the repair target, as of base b56a7f3.
`crates/format/tests/parts.rs` repairs each and reads the result back, or holds it to the
refusal it gets.

| File | Crash input | What repair did |
| --- | --- | --- |
| `kept-last-pages.tickwave` | `p6-short/repair/crash-2dd102a3be586eb52ff1655b0985a5d2e99c4a60` | Two digital channels, a block each and a one-row index page each, cut before the footer. Repair kept the pages and wrote last pages again, so the top index's entry counts were wrong. |
| `bundle-before-the-cut.tickwave` | `p6-short/repair/crash-b24561364989ecae8e8f03b29f66a2c23524df67` | A file cut at its footer, its bundle written. Repair kept the bundle and wrote records after it. Its bundle decodes to a JSON object followed by a footer's bytes, which a reader in order refuses. |
| `known-through-before-a-block-end.tickwave` | `p6-short/repair/crash-3f7dfa0aa4e88245da5b8dc3a08b111abba3d784`, its derived entry, run records and annotation records rewritten in the v1 forms | A known-through record at tick 0 after blocks that end at 480,000. Repair took the length from the record, past which the overview had settled, and `Ladder::finish` panicked (overview.rs:1489 at 46e61a4). A reader in order refuses the record. |

The crash inputs themselves are kept outside the repository.
