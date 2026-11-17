// Where a row pales what the instrument lost: a channel's own lists, or, for one whose lists are in
// pages, each tile's inside the tile's span, with the samples lost before the first from the
// header. Expected spans worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import { lostIn } from "../src/render/plot.ts";
import type { Layer, Tile } from "../src/render/track.ts";
import type { AnalogChannel, DigitalChannel, Listed } from "../src/source.ts";

/** Samples every 10 ticks from tick 100, 3 lost before, 2 after; 1,000 of them. */
const a: AnalogChannel = { kind: "analog", name: "A", end: 10_100, unit: "V", first: 100, period: { num: 10, den: 1 }, count: 1000, resolution: null, lostBefore: 3, lostAfter: 2 };

/** A samples tile over [start, end) whose answer carries `listed`. */
const layer = (start: number, end: number, listed?: Listed): Layer => {
  const tile: Tile = { kind: "samples", start, end, data: { first: 0, values: new Float64Array(0), ...(listed && { listed }) } };
  return { tile, start, end };
};

test("a channel whose lists are in its header pales them whatever its tiles carry", () => {
  // The break's 2 lost: places 500 and 502 of the first run, 5,100 to 5,120. Places -3 to -1 on the
  // first run's grid, 70 to 100. After sample 999, places 1,000 and 1,002 of the run from 6,000
  // (index 500): 11,000 to 11,020.
  const listed = { breaks: [{ index: 500, tick: 6000, lost: 9 }] };
  assert.deepEqual(lostIn({ ...a, breaks: [{ index: 500, tick: 6000, lost: 2 }] }, [layer(0, 20_000, listed)]), [
    { from: 5100, to: 5120, lost: 2 },
    { from: 70, to: 100, lost: 3 },
    { from: 11_000, to: 11_020, lost: 2 },
  ]);
});

test("a channel whose breaks are in pages pales each tile's, inside the tile, and the loss before its first from its header", () => {
  const paged: AnalogChannel = { ...a, paged: true, end: 20_000 };
  // The tile from 4,000 to 8,000 knows the break at sample 500, tick 6,000, after 2 lost: places
  // 500 and 502 of the first run, 5,100 to 5,120. It doesn't reach the end, so no loss after.
  const middle = layer(4000, 8000, { breaks: [{ index: 500, tick: 6000, lost: 2 }] });
  assert.deepEqual(lostIn(paged, [middle]), [
    { from: 70, to: 100, lost: 3 },
    { from: 5100, to: 5120 },
  ]);
  // A tile that reaches the end has the last run and the place after it: sample 999 of the run from
  // 6,000 (index 500) ends at 6,000 + 500 × 10 = 11,000, and two lost run to 11,020. A tile ends a
  // span: the part past it is not its.
  const end = layer(8000, 11_010, { breaks: [{ index: 500, tick: 6000, lost: 2 }], end: 11_000 });
  assert.deepEqual(lostIn(paged, [middle, end]), [
    { from: 70, to: 100, lost: 3 },
    { from: 5100, to: 5120 },
    { from: 11_000, to: 11_010 },
  ]);
  // A tile with nothing listed, as the strip a first draw takes, pales only the header's.
  assert.deepEqual(lostIn(paged, [layer(0, 20_000)]), [{ from: 70, to: 100, lost: 3 }]);
});

test("a digital channel whose gaps are in pages pales each tile's gaps inside it", () => {
  const d: DigitalChannel = { kind: "digital", name: "D", end: 1000, paged: true };
  const gaps = [
    { from: 100, to: 200, level: 1 as const },
    { from: 300, to: 400, level: 0 as const },
  ];
  assert.deepEqual(lostIn(d, [layer(150, 350, { gaps })]), [
    { from: 150, to: 200 },
    { from: 300, to: 350 },
  ]);
  // Its own gaps where they are in its header, whatever a tile carries.
  assert.deepEqual(lostIn({ ...d, paged: false, gaps: [gaps[0]] }, [layer(150, 350, { gaps })]), [gaps[0]]);
});
