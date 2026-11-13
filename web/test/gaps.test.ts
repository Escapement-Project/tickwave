// Gaps and breaks as FORMAT.md defines them: a digital channel's unknown spans, and an analog
// channel's runs and the samples it lost between them. Expected values are worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import { channelIndexAtOrAfter, channelSampleTick, inChannel, knownSpans, lostSpans, sampleAtOrBefore, type AnalogChannel, type DigitalChannel } from "../src/source.ts";

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

test("the first sample at or after a tick, across a break", () => {
  // Nothing of the first run lies at or after 7, so the second run's first sample.
  assert.deepEqual([6, 7, 29, 30, 31, 32, 36, 37].map((t) => channelIndexAtOrAfter(w, t)), [4, 5, 5, 5, 6, 7, 9, 10]);
  // The value a cursor reads: the last sample at or before it.
  assert.deepEqual([6, 29, 30, 35].map((t) => sampleAtOrBefore(w, t)), [4, 4, 5, 8]);
});

test("the lost samples' time: a period after the sample before, for as many periods as were lost", () => {
  // Sample 4 at 6, so from 7.5, for two periods of 1.5: to 10.5, well before the run at 30.
  assert.deepEqual(lostSpans(w), [{ from: 7.5, to: 10.5, lost: 2 }]);
  // Lost more than fit before the run starts again: the span ends there.
  assert.deepEqual(lostSpans({ ...w, breaks: [{ index: 5, tick: 9, lost: 4 }] }), [{ from: 7.5, to: 9, lost: 4 }]);
  // A pause loses nothing.
  assert.deepEqual(lostSpans({ ...w, breaks: [{ index: 5, tick: 30, lost: 0 }] }), []);
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
