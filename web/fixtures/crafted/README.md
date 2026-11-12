# Crafted recordings

Files built to break a reader that trusts the lengths and counts a recording states. Each must be
refused with an error, never by a panic, which in WASM ends the module and every recording on the
page. `test/wasm.test.ts` runs them through the browser's decoder. Each is a real recording with
one thing changed and its footer's check made again, so a reader meets what the change says.
`cargo run -p escapement-tickwave-format --example craft -- web/fixtures/crafted`, from the
repository, writes them.

| File | What it states |
| --- | --- |
| `header-wrap.tickwave` | a footer whose header length is near 4 GiB, which wraps a 32-bit sum of its lengths |
| `levels.tickwave` | the same overview level 2,048 times, which together decode to more than WASM can hold |
| `big-decoded.tickwave` | a block whose index row says it decodes to 2 GiB |
| `huge-count.tickwave` | 2^28 + 1 edges in a block whose section holds three |
| `sections-wrap.tickwave` | section lengths whose 32-bit sum wraps to the block's size |
