// An analog row's value range as the reader zooms and moves it, from testdata/view/scale.json,
// which the Rust view core's tests read too. Expected values are worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import { clampRange, fitRange, moveRange, rangeMarks, scaleLimits, zoomRange, type ValueRange } from "../src/render/scale.ts";
import { runTable } from "./tables.ts";

/** The limits a row's `limits` names: what scaleLimits gives for its `whole` and `resolution`. */
const limits = (l: { whole: ValueRange; resolution: number | null }) => scaleLimits(l.whole, l.resolution);

runTable("scale", {
  scale_limits: (a) => {
    const l = limits(a as { whole: ValueRange; resolution: number | null });
    return { whole: l.whole, min_span: l.minSpan, max_span: l.maxSpan };
  },
  zoom_range: (a) => zoomRange(a.range, a.at, a.factor, limits(a.limits)),
  move_range: (a) => moveRange(a.range, a.by, limits(a.limits)),
  clamp_range: (a) => clampRange(a.range, limits(a.limits)),
  fit_range: (a) => fitRange(a.min, a.max, limits(a.limits)),
  range_marks: (a) => rangeMarks(a.range),
});

test("rangeMarks: across a sweep of ranges, labels inside each, within a quarter of it from its edge", () => {
  for (let k = 0; k < 500; k++) {
    const lo = Math.sin(k) * 10 ** ((k % 7) - 3);
    const hi = lo + 10 ** ((k % 5) - 4) * (1 + (k % 13));
    const { top, bottom } = rangeMarks({ lo, hi });
    assert.ok(bottom >= lo - 1e-9 * (hi - lo) && top <= hi + 1e-9 * (hi - lo), `${bottom} to ${top} outside ${lo} to ${hi}`);
    assert.ok(hi - top <= (hi - lo) / 4 && bottom - lo <= (hi - lo) / 4, `${lo} to ${hi}: ${bottom} and ${top}`);
  }
});
