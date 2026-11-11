// Decodes a recording through the WASM build and prints each digital channel's edge count, tick
// sum and XOR, for comparison against values computed independently from the source. Where the
// recording has overview blocks, it also checks the finest level against the blocks: a digital
// channel's change bits against its edges, an analog channel's minimums and maximums against its
// samples.
//   node crates/wasm/check.mjs FILE.tickwave
import { readFileSync } from "node:fs";
import { initSync, headLength, Recording } from "./pkg/escapement_tickwave_wasm.js";

initSync({ module: readFileSync(new URL("./pkg/escapement_tickwave_wasm_bg.wasm", import.meta.url)) });
const file = readFileSync(process.argv[2]);
const head = file.subarray(0, headLength(file.subarray(0, 65536)));
const rec = new Recording(head);
const header = JSON.parse(rec.header);
const finest = header.overview_blocks?.[0];
const sums = header.channels.map(() => ({ count: 0, sum: 0n, xor: 0n, buckets: new Set() }));
const buckets = finest ? Math.floor(header.length / 2 ** finest.shift) + 1 : 0;
const extremes = header.channels.map((c) => {
  if (!finest || c.kind !== "analog") return null;
  const [num, den] = c.period;
  if (c.count * num >= 2 ** 53) throw new Error(`${c.name}: sample ticks pass 2^53`);
  return {
    num,
    den,
    min: new Float32Array(buckets).fill(Infinity),
    max: new Float32Array(buckets).fill(-Infinity),
  };
});
for (let i = 0; i < rec.blockCount; i++) {
  const [from, to] = rec.blockRange(i);
  const block = rec.decodeBlock(i, file.subarray(from, to));
  header.channels.forEach((c, k) => {
    const e = extremes[k];
    if (e) {
      const first = block.firstIndex(k);
      block.values(k).forEach((v, j) => {
        if (Number.isNaN(v)) return;
        const tick = c.first + Math.floor(((first + j) * e.num) / e.den);
        const b = Math.floor(tick / 2 ** finest.shift);
        if (v < e.min[b]) e.min[b] = v;
        if (v > e.max[b]) e.max[b] = v;
      });
    }
    if (c.kind !== "digital") return;
    for (const t of block.edges(k)) {
      sums[k].count++;
      sums[k].sum += BigInt(t);
      sums[k].xor ^= BigInt(t);
      if (finest) sums[k].buckets.add(Math.floor(t / 2 ** finest.shift));
    }
  });
  block.free();
}
header.channels.forEach((c, k) => console.log(c.name, sums[k].count, sums[k].sum, sums[k].xor));
if (finest) {
  const changed = header.channels.map(() => 0);
  for (let b = 0; b < finest.blocks; b++) {
    const [from, to] = rec.overviewBlockRange(0, b);
    const block = rec.decodeOverviewBlock(0, b, file.subarray(from, to));
    header.channels.forEach((c, k) => {
      const e = extremes[k];
      if (e) {
        const [min, max] = block.analog(k);
        for (let i = 0; i < min.length; i++) {
          const b = block.first + i;
          if (min[i] !== e.min[b] || max[i] !== e.max[b]) {
            console.log(`${c.name}: bucket ${b} holds ${min[i]}..${max[i]}, the samples ${e.min[b]}..${e.max[b]}`);
            process.exit(1);
          }
        }
        e.checked = (e.checked ?? 0) + min.length;
      }
      if (c.kind !== "digital") return;
      const [, change] = block.digital(k);
      for (const byte of change) for (let bit = byte; bit; bit &= bit - 1) changed[k]++;
    });
    block.free();
  }
  let ok = true;
  header.channels.forEach((c, k) => {
    const e = extremes[k];
    if (e) {
      console.log(`${c.name}: 2^${finest.shift} minimums and maximums match the samples in ${e.checked} of ${buckets} buckets`);
      ok &&= e.checked === buckets;
    }
    if (c.kind !== "digital") return;
    const want = sums[k].buckets.size;
    console.log(`${c.name}: 2^${finest.shift} buckets with a change: ${changed[k]}, from edges: ${want}`);
    ok &&= changed[k] === want;
  });
  if (!ok) process.exit(1);
}
