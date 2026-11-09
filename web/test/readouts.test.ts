// Expected values are worked by hand from the timebase and the ticks, not by running the code.
// Edges in the period rows are real: identify-swd's SWDIO and SWCLK around the first DPIDR read,
// as an independent decode of the Saleae export printed them.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clockNote,
  countEdges,
  formatClockPeriod,
  formatClockRate,
  formatCount,
  formatInterval,
  formatRate,
  formatValue,
  periodStats,
  tickPlace,
} from "../src/readouts.ts";

const GS = { num: 1_000_000_000, den: 1 };
const nb = (s: string) => s.replace(/ /g, " ").replace(/-/g, "−");

test("tickPlace: the largest power of ten not above one tick", () => {
  assert.equal(tickPlace(GS), -9); // 1 ns
  assert.equal(tickPlace({ num: 100_000, den: 1 }), -5); // 10 us
  assert.equal(tickPlace({ num: 200_000, den: 1 }), -6); // 5 us: 1 us is the largest power below
  assert.equal(tickPlace({ num: 3_000_000, den: 1 }), -7); // 333.3 ns: 100 ns
  assert.equal(tickPlace({ num: 1, den: 3 }), 0); // 3 s ticks: 1 s
});

test("formatInterval: digits down to the tick and no further", () => {
  // The brief's own example: at 1 ns, 1.234567 ms, never 1.2345670 ms.
  assert.equal(formatInterval(1_234_567, GS), nb("1.234567 ms"));
  // A trailing zero at the ns digit is a measured digit and stays.
  assert.equal(formatInterval(1_234_570, GS), nb("1.234570 ms"));
  assert.equal(formatInterval(50, GS), nb("50 ns"));
  assert.equal(formatInterval(999, GS), nb("999 ns"));
  assert.equal(formatInterval(1000, GS), nb("1.000 us").replace("us", "µs"));
  assert.equal(formatInterval(0, GS), nb("0 ns"));
  assert.equal(formatInterval(-1500, GS), nb("-1.500 us").replace("us", "µs"));
  assert.equal(formatInterval(1_000_000_000, GS), nb("1.000000000 s"));
  // 123 ticks of 10 us is 1.23 ms.
  assert.equal(formatInterval(123, { num: 100_000, den: 1 }), nb("1.23 ms"));
  // 247 ticks of 5 us is 1.235 ms, exact at the us digit.
  assert.equal(formatInterval(247, { num: 200_000, den: 1 }), nb("1.235 ms"));
  // 4 ticks at 3 MS/s is 1.333... us; the tick is 333 ns, so the last digit is 100 ns. One digit
  // more (1.33 us) or fewer (1 us) is the off-by-one this row catches.
  assert.equal(formatInterval(4, { num: 3_000_000, den: 1 }), nb("1.3 us").replace("us", "µs"));
});

test("formatRate: digits from a span uncertain by about one tick", () => {
  // One 40 ns period: 25 MHz, uncertain by 25 MHz / 40 = 625 kHz, so the last digit is 100 kHz.
  // Rounding the place up instead of down would print 25 MHz.
  assert.equal(formatRate(1, 40, GS), nb("25.0 MHz"));
  // 1000 periods over 40000 ticks: 25 MHz, uncertain by 625 Hz: last digit 100 Hz.
  assert.equal(formatRate(1000, 40_000, GS), nb("25.0000 MHz"));
  // One 1 us period: 1 MHz, uncertain by exactly 1 kHz, a power of ten: last digit 1 kHz.
  // A "smaller than" rule instead of "not larger than" would print 1.0000 MHz.
  assert.equal(formatRate(1, 1000, GS), nb("1.000 MHz"));
  // 45 periods over 45016 ticks: 999,644.57 Hz, uncertain by 22.2 Hz: to 10 Hz, 999,640 Hz.
  assert.equal(formatRate(45, 45_016, GS), nb("999.64 kHz"));
  assert.equal(formatRate(0, 100, GS), null);
  assert.equal(formatRate(3, 0, GS), null);
});

test("periodStats: rising edge to rising edge, starting low", () => {
  // SWDIO, low before the first edge. Rising at 377094556, 377096556, 377099556, 377101556,
  // 377106074: periods 2000, 3000, 2000, 4518. Sorted 2000, 2000, 3000, 4518; the lower middle
  // is 2000 (the mean of the middle two, 2500, did not occur). Falling-to-falling would give
  // three periods with a maximum of 3522; counting edges would give 9 or 8.
  // Within one tick of the median: the first and third, two runs of one, 4000 ticks.
  const ticks = [377094556, 377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074];
  assert.deepEqual(periodStats(0, ticks), { count: 4, median: 2000, min: 2000, max: 4518, used: 2, runs: 2, span: 4000 });
});

test("periodStats: starting high, the first edge falls", () => {
  // The same SWDIO from its second edge, high before it. Rising at 377096556, 377099556,
  // 377101556, 377106074: periods 3000, 2000, 4518; median 3000, which alone is within a tick of
  // itself. Taking the first edge as rising measures the falling edges instead: 2000, 3000, 3522.
  const ticks = [377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074];
  assert.deepEqual(periodStats(1, ticks), { count: 3, median: 3000, min: 2000, max: 4518, used: 1, runs: 1, span: 3000 });
});

test("periodStats: SWCLK at 1 MHz, and fewer than two rising edges", () => {
  // SWCLK idles high; rising at 377095057, 377096057, 377097058, 377098058.
  const clk = [377094557, 377095057, 377095557, 377096057, 377096557, 377097058, 377097558, 377098058];
  assert.deepEqual(periodStats(1, clk), { count: 3, median: 1000, min: 1000, max: 1001, used: 3, runs: 1, span: 3001 });
  assert.equal(periodStats(1, clk.slice(0, 3)), null);
  assert.equal(periodStats(0, []), null);
});

/** Rising edges at `rises`, each high for half its period: the edges a clock row would hold. */
function clockEdges(rises: number[]): number[] {
  const out: number[] = [];
  rises.forEach((r, i) => {
    out.push(r);
    if (i + 1 < rises.length) out.push(r + Math.floor((rises[i + 1] - r) / 2));
  });
  return out;
}

test("the running clock: a quantised 26.67 ns clock", () => {
  // A 26.67 ns clock sampled at 1 ns reads as 26 ns ten times and 27 ns twenty-two times, here
  // 27, 27, 26 over and over. The median is 27 ns (37.04 MHz). All 32 periods are within a tick
  // of it, one run of 854 ticks: 32 / 854 ns is 37,470,726 Hz, uncertain by 1/854 of that,
  // 43.9 kHz, so to 10 kHz: 37.47 MHz. The mean period, 854 / 32 = 26.6875 ns, is uncertain by
  // 1/32 ns: to 0.01 ns, 26.69 ns.
  const periods = [...Array.from({ length: 10 }, () => [27, 27, 26]).flat(), 27, 27];
  const rises = [1000];
  for (const p of periods) rises.push(rises[rises.length - 1] + p);
  const s = periodStats(0, clockEdges(rises))!;
  assert.deepEqual(s, { count: 32, median: 27, min: 26, max: 27, used: 32, runs: 1, span: 854 });
  assert.equal(formatClockRate(s, GS), nb("37.47 MHz"));
  assert.equal(formatClockPeriod(s, GS), nb("26.69 ns"));
  assert.equal(clockNote(s), "mean of 32 periods");
});

test("the running clock: a span with a gap between bursts", () => {
  // Real SWCLK: the end of the DPIDR read's burst and the start of the next transfer's. Periods
  // 1000 four times, the 90,918-tick gap, then 1000 four times. First-to-last would be 9 periods
  // over 98,918 ticks, 90.98 kHz. The running clock takes the eight 1000s, two runs of 8000 ticks
  // in all: 1 MHz, uncertain by 2/8000 of it, 250 Hz, so to 100 Hz. Its period, 1000 ticks, is
  // uncertain by 2/8 of a tick: to 0.1 ns.
  const ticks = [
    377135559, 377136060, 377136559, 377137060, 377137560, 377138060, 377138560, 377139060, 377139560, 377140060,
    377230478, 377230978, 377231478, 377231978, 377232478, 377232978, 377233478, 377233978, 377234478, 377234978,
  ];
  const s = periodStats(1, ticks)!;
  assert.deepEqual(s, { count: 9, median: 1000, min: 1000, max: 90918, used: 8, runs: 2, span: 8000 });
  assert.equal(formatClockRate(s, GS), nb("1.0000 MHz"));
  assert.equal(formatClockPeriod(s, GS), nb("1.0000 us").replace("us", "\u00b5s"));
  assert.equal(clockNote(s), "mean of 8 of 9 periods");
});

test("formatValue: to the digit of one code step", () => {
  // verge's energy capture: integer mV, read as V.
  assert.equal(formatValue(3305 * 0.001, 0.001, "V"), nb("3.305 V"));
  // integer nA, read as A: 32898465 nA is 32.898465 mA.
  assert.equal(formatValue(32898465 * 1e-9, 1e-9, "A"), nb("32.898465 mA"));
  // An f32 source states no resolution: four significant digits. 0.00012345 A rounds half away
  // from zero to 123.5 uA.
  assert.equal(formatValue(0.00012345, null, "A"), nb("123.5 uA").replace("uA", "µA"));
});

test("countEdges and formatCount", () => {
  const t = [10, 20, 30, 40];
  assert.equal(countEdges(t, 20, 40), 2);
  assert.equal(countEdges(t, 0, 10), 0);
  assert.equal(formatCount(15286), "15,286");
});
