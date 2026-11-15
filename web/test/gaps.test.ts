// Gaps and breaks as FORMAT.md defines them: a digital channel's unknown spans, and an analog
// channel's runs and the samples it lost between them. Expected values are worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  channelExtent,
  channelIndexAtOrAfter,
  channelSampleTick,
  endLosses,
  inChannel,
  knownSpans,
  listedOnAxis,
  lostSpans,
  placeAfter,
  sampleAtOrBefore,
  spanned,
  type AnalogChannel,
  type DigitalChannel,
} from "../src/source.ts";

/** Samples 0 to 4 from tick 0, every 3/2 ticks (ticks 0, 1, 3, 4, 6); two lost; then samples 5
 * to 9 from tick 30 (ticks 30, 31, 33, 34, 36). */
const w: AnalogChannel = {
  kind: "analog",
  name: "W",
  end: 40,
  unit: "V",
  first: 0,
  period: { num: 3, den: 2 },
  count: 10,
  breaks: [{ index: 5, tick: 30, lost: 2 }],
  resolution: null,
  min: 0,
  max: 9,
};

test("a sample's tick follows the run that holds it", () => {
  assert.deepEqual(
    Array.from({ length: 10 }, (_, i) => channelSampleTick(w, i)),
    [0, 1, 3, 4, 6, 30, 31, 33, 34, 36],
  );
});

test("a run's data ends at the place after its last sample on its grid", () => {
  // Place 5 of the first run is floor(7.5) = 7, where sample 4's time ends: not 6 + ceil(1.5).
  assert.deepEqual(knownSpans(w, 0, 40), [
    [0, 7],
    [30, 40],
  ]);
  // The second run's last sample, 9, ends at 30 + floor(5 × 1.5) = 37.
  assert.deepEqual([4, 9].map((i) => placeAfter(w, i)), [7, 37]);
});

test("the first sample at or after a tick, across a break", () => {
  // Nothing of the first run lies at or after 7, so the second run's first sample.
  assert.deepEqual([6, 7, 29, 30, 31, 32, 36, 37].map((t) => channelIndexAtOrAfter(w, t)), [4, 5, 5, 5, 6, 7, 9, 10]);
  // The value a cursor reads: the last sample at or before it.
  assert.deepEqual([6, 29, 30, 35].map((t) => sampleAtOrBefore(w, t)), [4, 4, 5, 8]);
});

test("the lost samples' time: where the run before the break would have put them", () => {
  // Places 5 and 7 of the first run's grid: floor(7.5) = 7 and floor(10.5) = 10, well before the
  // run at 30. A period after sample 4, at 6, would start them at 7.5.
  assert.deepEqual(lostSpans(w), [{ from: 7, to: 10, lost: 2 }]);
  // Lost more than fit before the run starts again: the span ends there.
  assert.deepEqual(lostSpans({ ...w, breaks: [{ index: 5, tick: 9, lost: 4 }] }), [{ from: 7, to: 9, lost: 4 }]);
  // The run starts again before place 5: the lost time is empty, at the break.
  assert.deepEqual(lostSpans({ ...w, breaks: [{ index: 5, tick: 6, lost: 1 }] }), [{ from: 6, to: 6, lost: 1 }]);
  // A pause loses nothing.
  assert.deepEqual(lostSpans({ ...w, breaks: [{ index: 5, tick: 30, lost: 0 }] }), []);
  // A second break's lost samples sit on the grid of the run at 30, from sample 5: places 8 and 11
  // are 30 + floor(4.5) = 34 and 30 + floor(9) = 39.
  const two = [{ index: 5, tick: 30, lost: 2 }, { index: 8, tick: 50, lost: 3 }];
  assert.deepEqual(lostSpans({ ...w, breaks: two }), [{ from: 7, to: 10, lost: 2 }, { from: 34, to: 39, lost: 3 }]);
});

test("a digital channel's data splits around its gaps", () => {
  const g: DigitalChannel = { kind: "digital", name: "G", end: 100, gaps: [{ from: 10, to: 20, level: 1 }, { from: 60, to: 70, level: 0 }] };
  assert.deepEqual(knownSpans(g, 0, 100), [
    [0, 10],
    [20, 60],
    [70, 100],
  ]);
  assert.deepEqual(knownSpans(g, 15, 65), [[20, 60]]);
  assert.deepEqual(knownSpans({ ...g, start: 5 }, 0, 12), [[5, 10]]);
  assert.deepEqual([9, 10, 19, 20].map((t) => inChannel(g, t)), [true, false, false, true]);
});

test("the samples lost before the first and after the last: on the first run's grid and the last's", () => {
  // Three lost before tick 10, every 3/2 ticks: places -3 to -1 on the first run's grid, from
  // 10 + floor(-4.5) = 5. Two lost after sample 9, the second run's fifth: on the grid of the run
  // at 30, from sample 5, places 10 to 12 run from 30 + floor(7.5) = 37 to 30 + floor(10.5) = 40.
  const lost: AnalogChannel = { ...w, first: 10, breaks: [{ index: 5, tick: 30, lost: 0 }], lostBefore: 3, lostAfter: 2 };
  assert.deepEqual(endLosses(lost), [
    { from: 5, to: 10, lost: 3 },
    { from: 37, to: 40, lost: 2 },
  ]);
  // A view pales them as it pales the samples lost at a break, and spans them.
  assert.deepEqual(lostSpans({ ...lost, breaks: [{ index: 5, tick: 30, lost: 2 }] }), [
    { from: 17, to: 20, lost: 2 },
    { from: 5, to: 10, lost: 3 },
    { from: 37, to: 40, lost: 2 },
  ]);
  assert.deepEqual(channelExtent({ ...lost, end: 37 }), { start: 5, end: 40 });
  // Without a break the last run is the first: places 10 and 12 from 10 are 25 and 28.
  assert.deepEqual(endLosses({ ...lost, breaks: [], lostBefore: undefined }), [{ from: 25, to: 28, lost: 2 }]);
  // None lost: the extent is the data's.
  assert.deepEqual(channelExtent(w), { start: 0, end: 40 });
});

test("a channel whose list is in pages takes an answer's, which holds for the answer's span", () => {
  const paged: AnalogChannel = { ...w, breaks: undefined, paged: true, end: 100, lostAfter: 2 };
  // Breaks around a span in the middle: the place after the last sample is not known, so nor is
  // the loss after it.
  const middle = spanned(paged, { breaks: [{ index: 5, tick: 30, lost: 2 }] });
  assert.ok(middle.kind === "analog");
  assert.deepEqual([middle.breaks, middle.end, middle.lostAfter], [[{ index: 5, tick: 30, lost: 2 }], 100, undefined]);
  // Without an answer the last run is not known, nor the loss after it.
  assert.deepEqual(endLosses(paged), []);
  // An answer that reaches the end gives it, and the loss after it follows.
  const end = spanned(paged, { breaks: [{ index: 5, tick: 30, lost: 2 }], end: 37 }) as AnalogChannel;
  assert.deepEqual([end.end, endLosses(end)], [37, [{ from: 37, to: 40, lost: 2 }]]);
  // A digital channel's gaps, and where its level is first known.
  const d: DigitalChannel = { kind: "digital", name: "D", end: 100, paged: true };
  const at = spanned(d, { gaps: [{ from: 40, to: 50, level: 1 }], start: 30 });
  assert.deepEqual(knownSpans(at, 0, 100), [
    [30, 40],
    [50, 100],
  ]);
  // A channel whose list is in its header is itself, whatever an answer carries.
  const g: DigitalChannel = { kind: "digital", name: "G", end: 100, gaps: [{ from: 10, to: 20, level: 1 }] };
  assert.equal(spanned(g, { gaps: [] }), g);
  // Moved along the axis, an answer's gaps and ends move with it; its breaks are the source's ticks.
  assert.deepEqual(listedOnAxis({ gaps: [{ from: 40, to: 50, level: 1 }], start: 30, end: 90, breaks: [{ index: 1, tick: 7, lost: 0 }] }, (t) => t + 5), {
    gaps: [{ from: 45, to: 55, level: 1 }],
    start: 35,
    end: 95,
    breaks: [{ index: 1, tick: 7, lost: 0 }],
  });
});
