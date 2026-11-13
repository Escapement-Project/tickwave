// A recording laid out as verge's energy captures are (fixtures/energy.tickwave, written by
// crates/format/examples/energy.rs): integer channels with exact scales, a derived power channel,
// a loss and a pause between runs, a gap and a pulse at tick 0 on a pin. Expected values are worked
// by hand from the example's rules.

import { test } from "node:test";
import assert from "node:assert/strict";
import { openTickwave } from "../scripts/node-wasm.ts";
import { lostSpans } from "../src/source.ts";

const FILE = "fixtures/energy.tickwave";

/** The example's V and I codes for pair `i`, in mV and nA. */
function pair(i: number): [number, number] {
  const step = Math.floor(i / 50) % 4;
  return [3300 - 5 * step + (i % 3), 1_000_000 + 250_000 * step + ((i * 7919) % 1000)];
}

test("its channels: integer readings to four figures, power after them, the runs and the loss", async () => {
  const r = await openTickwave(FILE);
  assert.deepEqual(
    r.info.channels.map((c) => c.name),
    ["V", "I", "range", "under_range", "DIO0", "P"],
  );
  assert.deepEqual(r.info.sources?.map((s) => [s.format, s.files, s.timebase, s.origin]), [["verge-energy", [], [96_000_000, 1], 0]]);
  const [v, i, , , dio, p] = r.info.channels;
  assert.ok(v.kind === "analog" && i.kind === "analog" && p.kind === "analog" && dio.kind === "digital");
  assert.deepEqual([v.resolution, i.resolution, p.resolution], [1 / 1000, 1 / 1e9, 1 / 1e12]);
  assert.deepEqual([v.significant, i.significant, p.unit], [4, 4, "W"]);
  // 1,000 pairs from tick 0 every 960 ticks, 50 lost, 800 from 1,008,000, a pause, 800 from 2,000,000.
  assert.deepEqual(v.breaks, [
    { index: 1000, tick: 1_008_000, lost: 50 },
    { index: 1800, tick: 2_000_000, lost: 0 },
  ]);
  assert.deepEqual(p.breaks, v.breaks);
  // The loss: from a period past pair 999 (959,040 + 960) for 50 periods, which ends at the next run.
  assert.deepEqual(lostSpans(v), [{ from: 960_000, to: 1_008_000, lost: 50 }]);
  assert.deepEqual(dio.gaps, [{ from: 960_000, to: 1_008_000, level: 1 }]);
  // The smallest and largest V: 3,285 mV (a step of 3, i % 3 = 0) and 3,302 mV.
  assert.deepEqual([v.min, v.max], [3285 / 1000, 3302 / 1000]);
});

test("its values come as the codes times their scales, and power as their product", async () => {
  const r = await openTickwave(FILE);
  // From 959,000 to 1,010,000: pair 999 at 959,040, then pairs 1,000 to 1,002 from 1,008,000.
  const s = await r.samples(0, 959_000, 1_010_000);
  assert.equal(s.first, 999);
  assert.deepEqual([...s.values], [999, 1000, 1001, 1002].map((k) => pair(k)[0] / 1000));
  const power = await r.samples(5, 959_000, 1_010_000);
  assert.deepEqual([...power.values], [999, 1000, 1001, 1002].map((k) => (pair(k)[0] * pair(k)[1]) / 1e12));
});

test("a pin's pulse at tick 0 keeps both edges, and its level after the gap is the gap's", async () => {
  const r = await openTickwave(FILE);
  const start = await r.edges(4, 0, 150_000);
  assert.deepEqual([start.level, [...start.ticks]], [0, [0, 0, 100_000]]);
  const after = await r.edges(4, 1_008_000, 1_600_000);
  assert.deepEqual([after.level, [...after.ticks]], [1, [1_500_000]]);
});
