// Column rules on real data: identify-swd's SWDIO and SWCLK (ticks as an independent decode of the
// Saleae export printed them) and the first twelve current samples of verge's energy capture (nA,
// 100 kHz, from the CSV). Expected states are worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  analogFromOverview,
  analogFromSamples,
  BUSY,
  digitalFromEdges,
  digitalFromOverview,
  FALL,
  HIGH,
  lineSamples,
  LOW,
  NONE,
  RISE,
  type Grid,
} from "../src/render/columns.ts";
import { MemorySource } from "../src/memory-source.ts";
import { fillRuns, sampleTick, SAMPLES_PER_RUN, type Ratio, type Samples } from "../src/source.ts";

const SWDIO = new Float64Array([377094556, 377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074]);

test("digital from edges: one edge a column rises or falls where it is", () => {
  const g = { x0: 377094000, per: 1000, count: 13 };
  const c = digitalFromEdges(g, { level: 0, ticks: SWDIO }, 377094000, 377107000);
  assert.deepEqual([...c.state], [RISE, FALL, RISE, FALL, LOW, RISE, FALL, RISE, HIGH, HIGH, FALL, LOW, RISE]);
  assert.equal(c.at[0], 0.556);
  assert.equal(c.at[10], 0.078);
  assert.equal(c.at[12], 0.074);
});

test("digital from edges: two or more edges a column are busy", () => {
  const g = { x0: 377094000, per: 2000, count: 7 };
  const c = digitalFromEdges(g, { level: 0, ticks: SWDIO }, 377094000, 377108000);
  assert.deepEqual([...c.state], [BUSY, BUSY, RISE, BUSY, HIGH, FALL, RISE]);
  assert.equal(c.at[2], 1556 / 2000);
});

test("digital from edges: nothing at or past the channel's end", () => {
  // The channel ends at 377105000; the edge at 377106074 is in the array but past the end.
  const g = { x0: 377094000, per: 2000, count: 7 };
  const c = digitalFromEdges(g, { level: 0, ticks: SWDIO }, 377094000, 377105000);
  assert.deepEqual([...c.state], [BUSY, BUSY, RISE, BUSY, HIGH, FALL, NONE]);
  // Ending at 377106050, inside the last column: the column is low up to the end, and the edge
  // at 377106074, past it in the same column, is not drawn.
  const d = digitalFromEdges(g, { level: 0, ticks: SWDIO }, 377094000, 377106050);
  assert.deepEqual([...d.state], [BUSY, BUSY, RISE, BUSY, HIGH, FALL, LOW]);
});

const identifyClock = () =>
  new MemorySource(
    {
      title: "SWCLK's first edges",
      timebase: { num: 1e9, den: 1 },
      length: 523583100,
      channels: [
        {
          kind: "digital",
          name: "SWCLK",
          end: 523583070,
          initial: 1,
          ticks: new Float64Array([376719436, 376720063, 376720710, 376721330, 376721977, 376722477]),
        },
      ],
    },
    { levels: [4096] },
  );

test("overview: level at each bucket's start, and whether it changes", async () => {
  // Buckets of 4096 from 376696832 (91967 * 4096). The first edge, 376719436, is in bucket 5,
  // [376717312, 376721408), with three more; bucket 6 holds the last two. SWCLK idles high.
  const ov = await identifyClock().overview(0, 376700000, 376725504, 4096);
  assert.ok(ov && ov.kind === "digital");
  assert.equal(ov.bucket, 4096);
  assert.equal(ov.start, 376696832);
  assert.deepEqual([...ov.level], [1, 1, 1, 1, 1, 1, 1]);
  assert.deepEqual([...ov.changes], [0, 0, 0, 0, 0, 1, 1]);
  assert.equal(await identifyClock().overview(0, 376700000, 376725504, 4095), null);
});

test("digital from overview: busy wherever a changing bucket overlaps", async () => {
  const ov = await identifyClock().overview(0, 376700000, 376725504, 4096);
  assert.ok(ov && ov.kind === "digital");
  const a = digitalFromOverview({ x0: 376701000, per: 3000, count: 7 }, ov, 376696832, 376725504);
  assert.deepEqual([...a.state], [HIGH, HIGH, HIGH, HIGH, HIGH, BUSY, BUSY]);
  // [376718000, 376721000) holds no bucket's start, only the tail of bucket 5, which changes.
  const b = digitalFromOverview({ x0: 376718000, per: 3000, count: 1 }, ov, 376696832, 376725504);
  assert.deepEqual([...b.state], [BUSY]);
});

test("digital from overview: a bucket wider than a column spans its columns", async () => {
  const ov = await identifyClock().overview(0, 376700000, 376725504, 4096);
  assert.ok(ov && ov.kind === "digital");
  // Four columns a bucket from bucket 4: bucket 4 holds no edge, 5 and 6 change.
  const c = digitalFromOverview({ x0: 376713216, per: 1024, count: 12 }, ov, 376696832, 376725504);
  assert.deepEqual([...c.state], [HIGH, HIGH, HIGH, HIGH, BUSY, BUSY, BUSY, BUSY, BUSY, BUSY, BUSY, BUSY]);
});

const CURRENT = [51480, 131360, 175990, 212221, 244081, 278385, 305817, 336396, 364408, 391567, 417043, 444567];

test("analog from samples: a column's range, joined to its neighbours", () => {
  // Three samples a column (ticks 5000 + 10000 i). Ranges [51480, 175990], [212221, 278385],
  // [305817, 364408], [391567, 444567]; each later one reaches down to the one before it.
  const c = analogFromSamples(
    { x0: 0, per: 30000, count: 4 },
    { first: 0, values: new Float64Array(CURRENT) },
    5000,
    { num: 10000, den: 1 },
    0,
    120000,
  );
  assert.deepEqual([...c.min], [51480, 175990, 278385, 364408]);
  assert.deepEqual([...c.max], [175990, 278385, 364408, 444567]);
});

test("analog overview from samples, then columns from it", async () => {
  const src = new MemorySource(
    {
      title: "current",
      timebase: { num: 1e9, den: 1 },
      length: 120000,
      channels: [
        { kind: "analog", name: "current", end: 120000, unit: "A", first: 5000, period: { num: 10000, den: 1 }, resolution: 1e-9, values: new Float64Array(CURRENT) },
      ],
    },
    { levels: [32768] },
  );
  // Buckets of 32768: samples 0-2, 3-6 (65000 < 65536), 7-9, 10-11.
  const ov = await src.overview(0, 0, 120000, 40000);
  assert.ok(ov && ov.kind === "analog");
  assert.deepEqual([...ov.min], [51480, 212221, 336396, 417043]);
  assert.deepEqual([...ov.max], [175990, 305817, 391567, 444567]);
  // A column and a half of buckets: column 0 overlaps buckets 0-1, column 1 buckets 1-2,
  // column 2 bucket 3. Columns 0 and 1 overlap already; column 2 reaches down to 391567.
  const c = analogFromOverview({ x0: 0, per: 49152, count: 3 }, ov, 0, 120000);
  assert.deepEqual([...c.min], [51480, 212221, 391567]);
  assert.deepEqual([...c.max], [305817, 391567, 444567]);
  // Two columns a bucket: each pair holds its bucket's range, and the first of the next pair
  // reaches down to it.
  const d = analogFromOverview({ x0: 0, per: 16384, count: 8 }, ov, 0, 120000);
  assert.deepEqual([...d.min], [51480, 51480, 175990, 212221, 305817, 336396, 391567, 417043]);
  assert.deepEqual([...d.max], [175990, 175990, 305817, 305817, 391567, 391567, 444567, 444567]);
});

// Every sample walked, as the rule reads: column c takes the samples whose ticks lie in
// [x0 + c * per, x0 + (c + 1) * per) and in [from, to), and then meets its neighbours.
function walked(g: Grid, s: Samples, first: number, period: Ratio, from: number, to: number) {
  const min = new Float64Array(g.count).fill(Infinity);
  const max = new Float64Array(g.count).fill(-Infinity);
  for (let k = 0; k < s.values.length; k++) {
    const t = sampleTick(first, period, s.first + k);
    if (t < from || t >= to) continue;
    for (let c = 0; c < g.count; c++) {
      if (t < g.x0 + c * g.per || t >= g.x0 + (c + 1) * g.per) continue;
      if (s.values[k] < min[c]) min[c] = s.values[k];
      if (s.values[k] > max[c]) max[c] = s.values[k];
    }
  }
  for (let c = 1; c < g.count; c++) {
    if (min[c] > max[c] || min[c - 1] > max[c - 1]) continue;
    if (max[c - 1] < min[c]) min[c] = max[c - 1];
    else if (min[c - 1] > max[c]) max[c] = min[c - 1];
  }
  return { min: [...min], max: [...max] };
}

// Uniform in [0, 1) from a fixed seed, so a failure repeats.
function seeded(seed: number): () => number {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 2 ** 32;
  };
}

test("analog from samples: each column holds exactly the samples whose ticks lie in it", () => {
  const random = seeded(0x7ac4);
  const int = (n: number) => Math.floor(random() * n);
  const periods: Ratio[] = [{ num: 1, den: 1 }, { num: 3, den: 2 }, { num: 1, den: 3 }, { num: 7, den: 1 }];
  for (let tile = 0; tile < 60; tile++) {
    const period = periods[tile % periods.length];
    const first = int(50);
    const values = new Float64Array(int(1500)).map(() => (random() < 0.03 ? NaN : Math.round(random() * 1000)));
    // A stretch of NaN longer than a run.
    if (tile % 5 === 0) values.fill(NaN, int(values.length), int(values.length) + 150);
    const s: Samples = { first: int(300), values };
    // The same tile as a source that sends its runs sends it.
    const n = Math.floor(values.length / SAMPLES_PER_RUN);
    const runs = { min: new Float32Array(n), max: new Float32Array(n) };
    fillRuns(values, runs.min, runs.max, 0, n);
    const sent: Samples = { ...s, runs };
    const tick = (i: number) => sampleTick(first, period, s.first + i);
    const span = tick(values.length) - tick(0) + 1;
    // Many grids over one tile: each reads the runs the ones before it built, and builds the rest.
    for (let grid = 0; grid < 25; grid++) {
      const per = 0.3 + random() * span / 4;
      const g: Grid = { x0: tick(0) - span / 4 + random() * span, per, count: 1 + int(40) };
      const from = tick(0) - 20 + int(span);
      const to = from + int(span);
      const want = walked(g, s, first, period, from, to);
      for (const [how, tileOf] of [["built", s], ["sent", sent]] as const) {
        const got = analogFromSamples(g, tileOf, first, period, from, to);
        assert.deepEqual({ min: [...got.min], max: [...got.max] }, want, `tile ${tile}, grid ${grid}, runs ${how}`);
      }
    }
  }
});

test("analog from samples: a column reads the runs its tile came with", () => {
  // One column over samples 0 to 199: three whole runs, and samples 192 to 199 one by one. The
  // runs say what no sample does, so the column shows they were read.
  const values = new Float64Array(200).fill(5);
  const runs = { min: new Float64Array([4, 3, 2]), max: new Float64Array([6, 7, 8]) };
  const c = analogFromSamples({ x0: 0, per: 200, count: 1 }, { first: 0, values, runs }, 0, { num: 1, den: 1 }, 0, 200);
  assert.deepEqual([c.min[0], c.max[0]], [2, 8]);
});

test("analog from samples: a draw builds the runs either side of one an earlier draw built", () => {
  // Ten runs, each sample its run's number, but run 4 holds the smallest and largest values.
  const values = new Float64Array(640).map((_, i) => Math.floor(i / 64));
  values[4 * 64 + 10] = -1;
  values[4 * 64 + 20] = 99;
  const s = { first: 0, values };
  const period = { num: 1, den: 1 };
  // One column over run 3 alone builds it and nothing else.
  const a = analogFromSamples({ x0: 192, per: 64, count: 1 }, s, 0, period, 0, 640);
  assert.deepEqual([a.min[0], a.max[0]], [3, 3]);
  const b = analogFromSamples({ x0: 0, per: 640, count: 1 }, s, 0, period, 0, 640);
  assert.deepEqual([b.min[0], b.max[0]], [-1, 99]);
});

test("line samples: those near the grid, out to one drawn sample past each side", () => {
  // Samples at ticks 0, 10, 20, ...; the grid covers [35, 65).
  const g = { x0: 35, per: 1, count: 30 };
  const period = { num: 10, den: 1 };
  const values = new Float64Array(12).fill(1);
  const s = { first: 0, values };
  // 30 is the last sample before the grid, 70 the first past it.
  assert.deepEqual(lineSamples(g, s, 0, period, 0, 1000, 0), [3, 8]);
  // A pad of 10 ticks reaches 25 and 75 (exclusive), so out to 20 and 80.
  assert.deepEqual(lineSamples(g, s, 0, period, 0, 1000, 10), [2, 9]);
  // NaN at 30 and 70: out to 20 and 80, the nearest the line can draw from.
  values[3] = NaN;
  values[7] = NaN;
  assert.deepEqual(lineSamples(g, s, 0, period, 0, 1000, 0), [2, 9]);
  // Nothing outside [from, to), NaN or not.
  assert.deepEqual(lineSamples(g, s, 0, period, 30, 70, 0), [3, 7]);
  // The tile's own indices, for a tile that starts at sample 2 (tick 20).
  assert.deepEqual(lineSamples(g, { first: 2, values: values.subarray(2) }, 0, period, 0, 1000, 0), [0, 7]);
});

test("line samples: every sample that can reach the grid, and one drawn sample past each side", () => {
  const random = seeded(0x51e7);
  const int = (n: number) => Math.floor(random() * n);
  const periods: Ratio[] = [{ num: 1, den: 1 }, { num: 3, den: 2 }, { num: 1, den: 3 }, { num: 7, den: 1 }];
  for (let tile = 0; tile < 60; tile++) {
    const period = periods[tile % periods.length];
    const first = int(50);
    const values = new Float64Array(int(400)).map(() => (random() < 0.2 ? NaN : random()));
    const s: Samples = { first: int(300), values };
    const tick = (i: number) => sampleTick(first, period, s.first + i);
    const span = tick(values.length) - tick(0) + 1;
    for (let grid = 0; grid < 25; grid++) {
      const g: Grid = { x0: tick(0) - span / 4 + random() * span, per: 0.05 + random() * 2, count: 1 + int(40) };
      const pad = random() < 0.3 ? 0 : random() * 30;
      const from = tick(0) - 20 + int(span);
      const to = from + int(span);
      const [k0, k1] = lineSamples(g, s, first, period, from, to, pad);
      // As the rule reads it: the drawn samples in [from, to) with ticks in the padded grid, the
      // last drawn one before it and the first at or past its end.
      const want: number[] = [];
      let before = -1;
      let after = -1;
      for (let k = 0; k < values.length; k++) {
        const t = tick(k);
        if (Number.isNaN(values[k]) || t < from || t >= to) continue;
        if (t < g.x0 - pad) before = k;
        else if (t < g.x0 + g.count * g.per + pad) want.push(k);
        else if (after < 0) after = k;
      }
      if (before >= 0) want.unshift(before);
      if (after >= 0) want.push(after);
      const got: number[] = [];
      for (let k = k0; k < k1; k++) if (!Number.isNaN(values[k])) got.push(k);
      assert.deepEqual(got, want, `tile ${tile}, grid ${grid}`);
    }
  }
});
