// Decodes a recording through the WASM build and prints each digital channel's edge count, tick
// sum and XOR, for comparison against values computed independently from the source. Where the
// recording has overview blocks, it also checks the finest level against the blocks: a digital
// channel's change bits against its edges, an analog channel's minimums and maximums against its
// samples' values.
//   node crates/wasm/check.mjs FILE.tickwave
import { readFileSync } from "node:fs";
import { initSync, footerStart, Recording } from "./pkg/escapement_tickwave_wasm.js";

initSync({ module: readFileSync(new URL("./pkg/escapement_tickwave_wasm_bg.wasm", import.meta.url)) });
const file = readFileSync(process.argv[2]);
const from = footerStart(file.subarray(Math.max(0, file.length - 32768)), file.length);
const rec = new Recording(file.subarray(from), file.length);
const header = JSON.parse(rec.header);
const finest = header.overview_blocks?.[0];
const sums = header.channels.map(() => ({ count: 0, sum: 0n, xor: 0n, buckets: new Set() }));
const buckets = finest ? Math.floor(header.length / 2 ** finest.shift) + 1 : 0;
// An f32 or codes channel's values come as f32; f64 and int channels' as f64.
const wide = (c) => c.coding.type === "f64" || c.coding.type === "int";
const extremes = header.channels.map((c) => {
  if (!finest || c.kind !== "analog") return null;
  const [num, den] = c.period;
  const runs = [[0, c.first], ...(c.breaks ?? []).map(([index, tick]) => [index, tick])];
  if (c.count * num >= 2 ** 53) throw new Error(`${c.name}: sample ticks pass 2^53`);
  const Floats = wide(c) ? Float64Array : Float32Array;
  return {
    // Sample i of the run that starts at [index, tick] sits at tick + floor((i - index) x period).
    tick: (i) => {
      let r = runs.length - 1;
      while (runs[r][0] > i) r--;
      return runs[r][1] + Math.floor(((i - runs[r][0]) * num) / den);
    },
    min: new Floats(buckets).fill(Infinity),
    max: new Floats(buckets).fill(-Infinity),
  };
});
for (let i = 0; i < rec.blockCount; i++) {
  const [from, to] = rec.blockRange(i);
  const block = rec.decodeBlock(i, file.subarray(from, to));
  header.channels.forEach((c, k) => {
    const e = extremes[k];
    if (e) {
      const first = block.firstIndex(k);
      (wide(c) ? block.values64(k) : block.values(k)).forEach((v, j) => {
        if (Number.isNaN(v)) return;
        const b = Math.floor(e.tick(first + j) / 2 ** finest.shift);
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
        const [min, max] = wide(c) ? block.analog64(k) : block.analog(k);
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
