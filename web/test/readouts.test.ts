// Expected values are worked by hand from the timebase and the ticks, not by running the code.
// Edges in the period rows are real: identify-swd's SWDIO and SWCLK around the first DPIDR read,
// as an independent decode of the Saleae export printed them.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  countEdges,
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
  const ticks = [377094556, 377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074];
  assert.deepEqual(periodStats(0, ticks), { count: 4, median: 2000, min: 2000, max: 4518, span: 11518 });
});

test("periodStats: starting high, the first edge falls", () => {
  // The same SWDIO from its second edge, high before it. Rising at 377096556, 377099556,
  // 377101556, 377106074: periods 3000, 2000, 4518, span 9518. Taking the first edge as rising
  // measures the falling edges instead: 2000, 3000, 3522.
  const ticks = [377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074];
  assert.deepEqual(periodStats(1, ticks), { count: 3, median: 3000, min: 2000, max: 4518, span: 9518 });
});

test("periodStats: SWCLK at 1 MHz, and fewer than two rising edges", () => {
  // SWCLK idles high; rising at 377095057, 377096057, 377097058, 377098058.
  const clk = [377094557, 377095057, 377095557, 377096057, 377096557, 377097058, 377097558, 377098058];
  assert.deepEqual(periodStats(1, clk), { count: 3, median: 1000, min: 1000, max: 1001, span: 3001 });
  assert.equal(periodStats(1, clk.slice(0, 3)), null);
  assert.equal(periodStats(0, []), null);
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
