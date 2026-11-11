// An analog row's value range as the reader zooms and moves it. Expected values are worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import { clampRange, fitRange, moveRange, rangeMarks, scaleLimits, zoomRange } from "../src/render/scale.ts";

const WHOLE = { lo: -2, hi: 6 };
const close = (a: number, b: number) => assert.ok(Math.abs(a - b) < 1e-9, `${a} is not ${b}`);
const range = (r: { lo: number; hi: number }, lo: number, hi: number) => {
  close(r.lo, lo);
  close(r.hi, hi);
};

test("scaleLimits: in to four code steps where the source has them, else a ten-thousandth; out to four times the whole", () => {
  assert.deepEqual(scaleLimits(WHOLE, 0.0025), { whole: WHOLE, minSpan: 0.01, maxSpan: 32 });
  const lim = scaleLimits(WHOLE, null);
  close(lim.minSpan, 0.0008);
  assert.equal(lim.maxSpan, 32);
});

test("zoomRange keeps the value under the pointer where it was", () => {
  const lim = scaleLimits(WHOLE, null);
  // 4 sits three quarters of the way up -2 to 6; halved, the range is 4 wide with 4 still there.
  range(zoomRange(WHOLE, 4, 0.5, lim), 1, 5);
  // In past the limit, the span stops at it, and 4 keeps its place.
  const deep = zoomRange(WHOLE, 4, 1e-9, lim);
  close(deep.hi - deep.lo, 0.0008);
  close((4 - deep.lo) / (deep.hi - deep.lo), 0.75);
});

test("zoomRange out stops at four times the whole, with its middle inside the whole", () => {
  const lim = scaleLimits(WHOLE, null);
  // About 6, the top: 32 wide would put the middle at -10; it moves up to -2, the whole's bottom.
  range(zoomRange(WHOLE, 6, 100, lim), -18, 14);
});

test("moveRange moves the values, as far as putting the middle at the whole's edge", () => {
  const lim = scaleLimits(WHOLE, null);
  range(moveRange({ lo: 1, hi: 3 }, 0.5, lim), 1.5, 3.5);
  range(moveRange({ lo: 1, hi: 3 }, 100, lim), 5, 7);
  range(moveRange({ lo: 1, hi: 3 }, -100, lim), -3, -1);
});

test("clampRange holds a span within the limits about its middle", () => {
  const lim = scaleLimits(WHOLE, 0.0025);
  range(clampRange({ lo: 2, hi: 2.001 }, lim), 1.9955, 2.0055);
  range(clampRange({ lo: -40, hi: 40 }, lim), -16, 16);
});

test("fitRange holds what was drawn, at least the least span, and nothing when nothing was", () => {
  const lim = scaleLimits(WHOLE, 0.0025);
  range(fitRange(1.2, 3.4, lim)!, 1.2, 3.4);
  range(fitRange(2, 2, lim)!, 1.995, 2.005);
  assert.equal(fitRange(Infinity, -Infinity, lim), null);
  assert.equal(fitRange(NaN, 1, lim), null);
});

test("rangeMarks labels round values inside the range, each within a quarter of it from its edge", () => {
  // 0.58 wide: steps of 0.1, so 3.5 at the top and 3.1 at the bottom, to one decimal place.
  const m = rangeMarks({ lo: 3.01, hi: 3.59 });
  close(m.top, 3.5);
  close(m.bottom, 3.1);
  assert.equal(m.place, -1);
  // A range on its steps labels its edges, though 3.6 / 0.2 is not quite 18 in binary.
  const edges = rangeMarks({ lo: 3, hi: 3.6 });
  close(edges.top, 3.6);
  close(edges.bottom, 3);
  for (let k = 0; k < 500; k++) {
    const lo = Math.sin(k) * 10 ** ((k % 7) - 3);
    const hi = lo + 10 ** ((k % 5) - 4) * (1 + (k % 13));
    const { top, bottom } = rangeMarks({ lo, hi });
    assert.ok(bottom >= lo - 1e-9 * (hi - lo) && top <= hi + 1e-9 * (hi - lo), `${bottom} to ${top} outside ${lo} to ${hi}`);
    assert.ok(hi - top <= (hi - lo) / 4 && bottom - lo <= (hi - lo) / 4, `${lo} to ${hi}: ${bottom} and ${top}`);
  }
});
