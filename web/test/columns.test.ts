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
  LOW,
  NONE,
  RISE,
} from "../src/render/columns.ts";
import { MemorySource } from "../src/memory-source.ts";

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
});
