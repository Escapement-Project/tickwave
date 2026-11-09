// Decodes a recording with the TypeScript reader (fzstd, XXH64 from hash-wasm) and prints each
// digital channel's edge count, tick sum and XOR, in crates/wasm/check.mjs's format.
//   node scripts/ts-sums.ts FILE.chrono
import { readFileSync } from "node:fs";
import { createXXHash64 } from "hash-wasm";
import { Head, headLength } from "../src/format.ts";
import { fzstdInflate, verify } from "../src/zstd.ts";

const hasher = await createXXHash64(0, 0);
const xxh = (d: Uint8Array) => {
  hasher.init();
  hasher.update(d);
  const b = hasher.digest("binary") as Uint8Array;
  return ((b[4] << 24) | (b[5] << 16) | (b[6] << 8) | b[7]) >>> 0;
};
const inflate = (f: Uint8Array, n: number) => {
  const out = fzstdInflate(f, n);
  verify(f, out, xxh);
  return out;
};
const file = new Uint8Array(readFileSync(process.argv[2]));
const head = new Head(file.subarray(0, headLength(file.subarray(0, 65536))));
head.attachOverview(inflate(head.overviewFrame, head.overviewLen));
const sums = head.header.channels.map(() => ({ count: 0, sum: 0n, xor: 0n }));
for (let i = 0; i < head.blockCount; i++) {
  const [from, to] = head.blockRange(i);
  const block = head.decodePayload(i, inflate(file.subarray(from, to), head.rows[i].decoded));
  block.channels.forEach((c, k) => {
    if (c.kind !== "digital") return;
    for (const t of c.edges) {
      sums[k].count++;
      sums[k].sum += BigInt(t);
      sums[k].xor ^= BigInt(t);
    }
  });
}
head.header.channels.forEach((c, k) => console.log(c.name, sums[k].count, sums[k].sum, sums[k].xor));
