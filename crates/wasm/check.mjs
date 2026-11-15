// Decodes a recording through the WASM build and prints each digital channel's edge count, tick
// sum and XOR, for comparison against values computed independently from the source. Where a
// channel's finest level is in overview blocks, it also checks that level against the channel's
// blocks: a digital channel's change bits against its edges, an analog channel's minimums and
// maximums against its samples' values.
//   node crates/wasm/check.mjs FILE.tickwave
import { readFileSync } from "node:fs";
import { initSync, footerStart, Recording } from "./pkg/escapement_tickwave_wasm.js";

initSync({ module: readFileSync(new URL("./pkg/escapement_tickwave_wasm_bg.wasm", import.meta.url)) });
const file = readFileSync(process.argv[2]);
const from = footerStart(file.subarray(Math.max(0, file.length - 32768)), file.length);
const rec = new Recording(file.subarray(from), file.length);
for (let ranges = rec.indexRanges(); ranges.length > 0; ranges = rec.indexRanges()) {
  for (let i = 0; i < ranges.length; i += 2) {
    rec.addIndexPage(ranges[i], file.subarray(ranges[i], ranges[i + 1]));
  }
}
const header = JSON.parse(rec.header);
// An f32 or codes channel's values come as f32; f64 and int channels' as f64.
const wide = (c) => c.coding.type === "f64" || c.coding.type === "int";
let ok = true;
header.channels.forEach((c, k) => {
  const shifts = rec.shifts(k);
  // The finest level, where it is in overview blocks rather than the strip.
  const finest = shifts.length > 1 ? shifts[0] : null;
  const buckets = finest === null ? 0 : Math.ceil(header.length / 2 ** finest);
  const sums = { count: 0, sum: 0n, xor: 0n, buckets: new Set() };
  let extremes = null;
  if (finest !== null && c.kind === "analog") {
    if (c.break_count) throw new Error(`${c.name}: its breaks are in list pages, which this doesn't read`);
    const [num, den] = c.period;
    const runs = [[0, c.first], ...(c.breaks ?? []).map(([index, tick]) => [index, tick])];
    if (c.count * num >= 2 ** 53) throw new Error(`${c.name}: sample ticks pass 2^53`);
    const Floats = wide(c) ? Float64Array : Float32Array;
    extremes = {
      // Sample i of the run that starts at [index, tick] sits at tick + floor((i - index) x period).
      tick: (i) => {
        let r = runs.length - 1;
        while (runs[r][0] > i) r--;
        return runs[r][1] + Math.floor(((i - runs[r][0]) * num) / den);
      },
      min: new Floats(buckets).fill(Infinity),
      max: new Floats(buckets).fill(-Infinity),
      checked: 0,
    };
  }
  for (let i = 0; i < rec.blockCount(k); i++) {
    const [from, to] = rec.blockRange(k, i);
    const block = rec.decodeBlock(k, i, file.subarray(from, to));
    if (extremes) {
      const first = block.firstIndex();
      (wide(c) ? block.values64() : block.values()).forEach((v, j) => {
        if (Number.isNaN(v)) return;
        const b = Math.floor(extremes.tick(first + j) / 2 ** finest);
        if (v < extremes.min[b]) extremes.min[b] = v;
        if (v > extremes.max[b]) extremes.max[b] = v;
      });
    }
    if (c.kind === "digital") {
      for (const t of block.edges()) {
        sums.count++;
        sums.sum += BigInt(t);
        sums.xor ^= BigInt(t);
        if (finest !== null) sums.buckets.add(Math.floor(t / 2 ** finest));
      }
    }
    block.free();
  }
  if (c.kind === "digital") console.log(c.name, sums.count, sums.sum, sums.xor);
  if (finest === null) return;
  let changed = 0;
  const [r0, r1] = rec.overviewRowsCovering(k, 0, 0, header.length);
  for (let r = r0; r < r1; r++) {
    const [from, to] = rec.overviewRowRange(k, 0, r);
    const block = rec.decodeOverviewRow(k, 0, r, file.subarray(from, to));
    // A row stands for `n` blocks alike, each of 4,096 buckets.
    for (let n = 0; n < block.n; n++) {
      const first = block.first + n * 4096;
      if (extremes) {
        const [min, max] = wide(c) ? block.analog64() : block.analog();
        for (let i = 0; i < min.length; i++) {
          const b = first + i;
          if (min[i] !== extremes.min[b] || max[i] !== extremes.max[b]) {
            console.log(`${c.name}: bucket ${b} holds ${min[i]}..${max[i]}, the samples ${extremes.min[b]}..${extremes.max[b]}`);
            process.exit(1);
          }
        }
        extremes.checked += min.length;
      }
      if (c.kind === "digital") {
        const [, change] = block.digital();
        for (const byte of change) for (let bit = byte; bit; bit &= bit - 1) changed++;
      }
    }
    block.free();
  }
  if (extremes) {
    console.log(`${c.name}: 2^${finest} minimums and maximums match the samples in ${extremes.checked} of ${buckets} buckets`);
    ok &&= extremes.checked === buckets;
  }
  if (c.kind === "digital") {
    console.log(`${c.name}: 2^${finest} buckets with a change: ${changed}, from edges: ${sums.buckets.size}`);
    ok &&= changed === sums.buckets.size;
  }
});
if (!ok) process.exit(1);
