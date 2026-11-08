// Decodes a recording through the WASM build and prints each digital channel's edge count, tick
// sum and XOR, for comparison against values computed independently from the source.
//   node crates/wasm/check.mjs FILE.chrono
import { readFileSync } from "node:fs";
import { initSync, headLength, Recording } from "./pkg/chrono_wasm.js";

initSync({ module: readFileSync(new URL("./pkg/chrono_wasm_bg.wasm", import.meta.url)) });
const file = readFileSync(process.argv[2]);
const head = file.subarray(0, headLength(file.subarray(0, 65536)));
const rec = new Recording(head);
const header = JSON.parse(rec.header);
const sums = header.channels.map(() => ({ count: 0, sum: 0n, xor: 0n }));
for (let i = 0; i < rec.blockCount; i++) {
  const [from, to] = rec.blockRange(i);
  const block = rec.decodeBlock(i, file.subarray(from, to));
  header.channels.forEach((c, k) => {
    if (c.kind !== "digital") return;
    for (const t of block.edges(k)) {
      sums[k].count++;
      sums[k].sum += BigInt(t);
      sums[k].xor ^= BigInt(t);
    }
  });
  block.free();
}
header.channels.forEach((c, k) => console.log(c.name, sums[k].count, sums[k].sum, sums[k].xor));
