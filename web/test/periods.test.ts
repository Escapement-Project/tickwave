// A span's period figures (readouts.ts `periodStats`, `PeriodCounter`): from a count of each
// distinct period, the figures they always were, a median and Tukey's fences at the sorted
// periods' indices; past `distinctAtMost` distinct periods a few numbers kept exactly, with no
// median and no fence.

import { test } from "node:test";
import assert from "node:assert/strict";
import { clockNote, periodStats } from "../src/readouts.ts";
import { sortedStats } from "./sorted-periods.ts";

function rng(seed: number) {
  let s = seed;
  return () => (s = (s * 1103515245 + 12345) % 2 ** 31) / 2 ** 31;
}

/** `periods` periods' edges, low before the first: a rising edge `between(k)` ticks after the
 * one before, each high for a tick. */
function clock(periods: number, between: (k: number) => number): Float64Array {
  const ticks = new Float64Array(2 * periods + 2);
  let t = 100;
  for (let k = 0; k <= periods; k++) {
    ticks[2 * k] = t;
    ticks[2 * k + 1] = t + 1;
    t += between(k);
  }
  return ticks;
}

test("a span's figures from its count of each distinct period are the sorted periods' figures, median and fences and all", () => {
  const r = rng(5);
  for (let i = 0; i < 300; i++) {
    const n = Math.floor(r() * 400);
    // Mostly a clock with jitter, now and then a pause.
    const ticks = clock(n, () => (r() < 0.05 ? 500 + Math.floor(r() * 5000) : 20 + Math.floor(r() * 4)));
    const level = r() < 0.5 ? 0 : 1;
    const cut = ticks.subarray(Math.floor(r() * 3));
    assert.deepEqual(periodStats(level, cut), sortedStats(level, cut), `case ${i}`);
  }
  // Counts merged batch after batch: a clock of few periods, and one of many distinct ones.
  for (const between of [(k: number) => (k % 100_000 === 7 ? 9_000 : 20 + (k % 3)), (k: number) => 20 + ((k * 7919) % 300_001)]) {
    const ticks = clock(1_000_000, between);
    assert.deepEqual(periodStats(0, ticks), sortedStats(0, ticks));
  }
});

test("a period met again just after the counts take in a full batch is counted", () => {
  // Eight periods the counter tallies at once, a batch of distinct ones, one that has the batch
  // merged, then the first of the eight again, many times: those last are the median.
  const order = [...Array.from({ length: 8 }, (_, k) => 10 + k), ...Array.from({ length: 2 ** 16 }, (_, k) => 1_000 + k), 10_000_000, ...Array(100_000).fill(10)];
  const ticks = clock(order.length, (k) => order[k]);
  const want = sortedStats(0, ticks)!;
  assert.deepEqual(periodStats(0, ticks), want);
  assert.equal(want.median, 10);
});

test("past the distinct periods it counts, a span's count, least, most and mean stay exact, with no median and no pause set aside", () => {
  // 100 distinct periods and a pause, against a count of 100.
  const between = (k: number) => (k === 1_234 ? 9_000 : 20 + (k % 100));
  const n = 5_000;
  const ticks = clock(n, between);
  let sum = 0;
  for (let k = 0; k < n; k++) sum += between(k);
  assert.deepEqual(periodStats(0, ticks, 100), { count: n, median: null, min: 20, max: 9_000, used: n, runs: 1, span: sum });
  // At 101, all counted: the sorted figures.
  assert.deepEqual(periodStats(0, ticks, 101), sortedStats(0, ticks));
  assert.equal(clockNote(periodStats(0, ticks, 100)!), "mean of 5,000 periods: no median, and pauses not set aside, past 2,097,152 distinct periods");
});
