// Column rules on real data: identify-swd's SWDIO and SWCLK (ticks as an independent decode of the
// Saleae export printed them) and the first twelve current samples of verge's energy capture (nA,
// 100 kHz, from the CSV). Expected states are worked by hand. The fixed cases are in
// testdata/view/columns.json, which the Rust view core's tests read too; the overviews a
// MemorySource builds, and the seeded sweeps, which the Rust tests port, are here.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  analogFromOverview,
  analogFromSamples,
  BUSY,
  digitalFromEdges,
  digitalFromLevels,
  FALL,
  HIGH,
  lineSamples,
  LOW,
  NONE,
  RISE,
  type DigitalColumns,
  type Grid,
} from "../src/render/columns.ts";
import { MemorySource } from "../src/memory-source.ts";
import { fillRuns, sampleTick, SAMPLES_PER_RUN, type Ratio, type Samples } from "../src/source.ts";
import { ratio, runTable, type Row } from "./tables.ts";

const STATES: Record<number, string> = { [NONE]: "NONE", [LOW]: "LOW", [HIGH]: "HIGH", [BUSY]: "BUSY", [RISE]: "RISE", [FALL]: "FALL" };
const digital = (c: DigitalColumns) => ({ state: [...c.state].map((s) => STATES[s]), at: [...c.at], edges: [...c.edges], high: [...c.high], known: [...c.known] });
const analog = (c: { min: Float64Array; max: Float64Array }) => ({ min: [...c.min], max: [...c.max] });
const edges = (e: { level: 0 | 1; ticks: number[] }) => ({ level: e.level, ticks: Float64Array.from(e.ticks) });
const runs = (r: { first: number; period: [number, number]; count: number }) => ({ first: r.first, period: ratio(r.period), count: r.count });
/** A row's tile, one object for every row of its test that names the same one. */
const tile = (a: Row["args"], shared: <T>(input: unknown, make: () => T) => T): Samples =>
  shared(a.samples, () => ({
    first: a.samples.first,
    values: Float64Array.from(a.samples.values),
    ...(a.samples.runs && { runs: { min: Float64Array.from(a.samples.runs.min), max: Float64Array.from(a.samples.runs.max) } }),
  }));

runTable("columns", {
  digital_from_edges: (a) => digital(digitalFromEdges(a.grid, edges(a.edges), a.from, a.to)),
  digital_from_overview: (a) =>
    digital(digitalFromLevels(a.grid, { bucket: a.overview.bucket, start: a.overview.start, density: Uint8Array.from(a.overview.density), duty: Uint8Array.from(a.overview.duty) }, a.from, a.to)),
  analog_from_samples: (a, { shared }) => analog(analogFromSamples(a.grid, tile(a, shared), runs(a.runs), a.from, a.to)),
  analog_from_overview: (a) =>
    analog(analogFromOverview(a.grid, { kind: "analog", bucket: a.overview.bucket, start: a.overview.start, min: Float64Array.from(a.overview.min), max: Float64Array.from(a.overview.max) }, a.from, a.to)),
  line_samples: (a, { shared }) => lineSamples(a.grid, tile(a, shared), runs(a.runs), a.from, a.to, a.pad),
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

test("overview: each bucket's density and duty, from its edges and its time high", async () => {
  // Buckets of 4096 from 376696832 (91967 * 4096). The first edge, 376719436, is in bucket 5,
  // [376717312, 376721408), with three more: density 1 + 4 log2(4) = 9. Bucket 6 holds the last
  // two: 5. SWCLK idles high, so buckets 0 to 4 are wholly high, 14. Bucket 5 is high for
  // 2124 + 647 + 78 = 2849 of its 4096 ticks, 14 × 2849 / 4096 = 9.74, so 10; bucket 6 for
  // 569 + 3027 = 3596, 12.29, so 12.
  const ov = await identifyClock().overview(0, 376700000, 376725504, 4096);
  assert.ok(ov && ov.kind === "digital");
  assert.equal(ov.bucket, 4096);
  assert.equal(ov.start, 376696832);
  assert.deepEqual([...ov.density], [0, 0, 0, 0, 0, 9, 5]);
  assert.deepEqual([...ov.duty], [14, 14, 14, 14, 14, 10, 12]);
  assert.equal(await identifyClock().overview(0, 376700000, 376725504, 4095), null);
  // The columns from these buckets are columns.json's "digital from overview" cases.
});

const CURRENT = [51480, 131360, 175990, 212221, 244081, 278385, 305817, 336396, 364408, 391567, 417043, 444567];

test("analog overview from samples", async () => {
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
  // The columns from these buckets are columns.json's "analog columns from an overview's buckets".
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
        const got = analogFromSamples(g, tileOf, { first, period, count: Infinity }, from, to);
        assert.deepEqual({ min: [...got.min], max: [...got.max] }, want, `tile ${tile}, grid ${grid}, runs ${how}`);
      }
    }
  }
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
      const [k0, k1] = lineSamples(g, s, { first, period, count: Infinity }, from, to, pad);
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
