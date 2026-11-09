// Expected values are worked by hand from the timebase and the ticks, not by running the code.
// Edges in the period rows are real: identify-swd's SWDIO and SWCLK around the first DPIDR read,
// as an independent decode of the Saleae export printed them.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clockNote,
  formatBrief,
  measureReading,
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
  // Tukey's fences: Q1 = p[0] = 2000, Q3 = p[3] = 4518, IQR 2518; all four inside, one run of
  // 11518 ticks.
  const ticks = [377094556, 377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074];
  assert.deepEqual(periodStats(0, ticks), { count: 4, median: 2000, min: 2000, max: 4518, used: 4, runs: 1, span: 11518 });
});

test("periodStats: starting high, the first edge falls", () => {
  // The same SWDIO from its second edge, high before it. Rising at 377096556, 377099556,
  // 377101556, 377106074: periods 3000, 2000, 4518, all inside the fences (Q1 2000, Q3 4518).
  // Taking the first edge as rising measures the falling edges instead: 2000, 3000, 3522.
  const ticks = [377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074];
  assert.deepEqual(periodStats(1, ticks), { count: 3, median: 3000, min: 2000, max: 4518, used: 3, runs: 1, span: 9518 });
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

/** Edges of a clock whose periods, rising to rising, are `periods`, from tick 1000. */
function clockOf(periods: number[]): number[] {
  const rises = [1000];
  for (const p of periods) rises.push(rises[rises.length - 1] + p);
  return clockEdges(rises);
}

test("Tukey's fences: the Escapement session's 50 MHz request keeps all seven periods", () => {
  // 19, 22, 17, 22, 20, 19, 22 ns (wire_swd/request/380567191 in gcc-swd-reads). Sorted
  // 17 19 19 20 22 22 22: Q1 = p[1] = 19, Q3 = p[5] = 22, IQR 3, fences [14.5, 26.5]. All seven,
  // one run of 141 ticks: 7 / 141 ns = 49.645 MHz, uncertain by 1/141 of it, 352 kHz: 49.6 MHz.
  // The one-tick window keeps 20, 19, 19 in two runs, 58 ticks, and reads 52 MHz.
  const s = periodStats(0, clockOf([19, 22, 17, 22, 20, 19, 22]))!;
  assert.deepEqual([s.used, s.runs, s.span], [7, 1, 141]);
  assert.equal(formatClockRate(s, GS), nb("49.6 MHz"));
});

test("Tukey's fences: one 80 ns period among 27 ns ones is dropped", () => {
  // 27 ×5, 80, 27 ×5. Sorted, Q1 = p[2] = 27, Q3 = p[8] = 27, IQR 1 (the floor): fences
  // [25.5, 28.5]. Ten kept, in two runs of 135: 10 / 270 ns = 37.04 MHz, uncertain by 2/270 of
  // it, 274 kHz: 37.0 MHz.
  const s = periodStats(0, clockOf([27, 27, 27, 27, 27, 80, 27, 27, 27, 27, 27]))!;
  assert.deepEqual([s.used, s.runs, s.span], [10, 2, 270]);
  assert.equal(formatClockRate(s, GS), nb("37.0 MHz"));
});

test("Tukey's fences: the IQR is at least one tick, so a tick of jitter stays", () => {
  // 27 ×5, 28, 27 ×5: Q1 = Q3 = 27. With IQR 0 the fences would be [27, 27] and drop the 28;
  // at one tick they are [25.5, 28.5]. All 11 kept, 298 ticks: 36.91 MHz, uncertain by 1/298,
  // 124 kHz: 36.9 MHz.
  const s = periodStats(0, clockOf([27, 27, 27, 27, 27, 28, 27, 27, 27, 27, 27]))!;
  assert.deepEqual([s.used, s.runs, s.span], [11, 1, 298]);
  assert.equal(formatClockRate(s, GS), nb("36.9 MHz"));
});

test("Tukey's fences: Q3 is p[⌈3(n − 1)/4⌉], not the floor", () => {
  // Six periods, sorted 20 20 20 20 30 50: Q1 = p[1] = 20, Q3 = p[⌈3.75⌉] = p[4] = 30, IQR 10,
  // fences [5, 45]: the 30 stays, the 50 goes. Q3 = p[3] = 20 would give IQR 1 and drop the 30.
  const s = periodStats(0, clockOf([20, 30, 20, 20, 50, 20]))!;
  assert.deepEqual([s.used, s.runs, s.span], [5, 2, 110]);
});

test("Tukey's fences: 93 and 100 ns alternating are both kept", () => {
  // Sorted 93 ×4, 100 ×4: Q1 = p[1] = 93, Q3 = p[6] = 100, IQR 7, fences [82.5, 110.5]. All
  // eight, one run of 772 ticks: 8 / 772 ns = 10.3627 MHz, uncertain by 1/772, 13.4 kHz:
  // 10.36 MHz. The one-tick window around the median (93) keeps only the 93s.
  const s = periodStats(0, clockOf([93, 100, 93, 100, 93, 100, 93, 100]))!;
  assert.deepEqual([s.used, s.runs, s.span], [8, 1, 772]);
  assert.equal(formatClockRate(s, GS), nb("10.36 MHz"));
});

test("Tukey's fences: a 33 and a 34 ns stretch among 27 ns periods are both dropped", () => {
  // 27 27 27 33 27 27 27 34 27 27 27 27. Sorted, Q1 = p[2] = 27, Q3 = p[9] = 27, IQR 1, fences
  // [25.5, 28.5]: both stretches out. A window of 25 % around the median, [20.25, 33.75], would
  // keep the 33 and drop the 34. Ten kept in three runs, 270 ticks: 37.04 MHz, uncertain by
  // 3/270 of it, 412 kHz: 37.0 MHz.
  const s = periodStats(0, clockOf([27, 27, 27, 33, 27, 27, 27, 34, 27, 27, 27, 27]))!;
  assert.deepEqual([s.used, s.runs, s.span], [10, 3, 270]);
  assert.equal(formatClockRate(s, GS), nb("37.0 MHz"));
});

test("the running clock: a quantised 26.67 ns clock", () => {
  // A 26.67 ns clock sampled at 1 ns reads as 26 ns ten times and 27 ns twenty-two times, here
  // 27, 27, 26 over and over. The median is 27 ns (37.04 MHz). Sorted, Q1 = p[7] = 26 and
  // Q3 = p[24] = 27, IQR 1, fences [24.5, 28.5]: all 32 periods inside, one run of 854 ticks: 32 / 854 ns is 37,470,726 Hz, uncertain by 1/854 of that,
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
  // over 98,918 ticks, 90.98 kHz. Q1 = Q3 = 1000, IQR 1, fences [998.5, 1001.5]: the running
  // clock takes the eight 1000s, two runs of 8000 ticks
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

test("formatBrief: three significant figures for a summary line, never finer than a tick", () => {
  assert.equal(formatBrief(927888190, GS), nb("928 ms"));
  assert.equal(formatBrief(523583100, GS), nb("524 ms"));
  assert.equal(formatBrief(114006396250, GS), nb("114 s"));
  assert.equal(formatBrief(4429986070, GS), nb("4.43 s"));
  assert.equal(formatBrief(11031886, GS), nb("11.0 ms"));
  // 999.6 ms rounds up into the next unit, still three figures: 1.00 s, not 1000 ms.
  assert.equal(formatBrief(999600000, GS), nb("1.00 s"));
  // 15 ticks of 1 ns: 15 ns, not 15.0 ns. 3 ticks of 10 us: 30 us.
  assert.equal(formatBrief(15, GS), nb("15 ns"));
  assert.equal(formatBrief(3, { num: 100_000, den: 1 }), nb("30 us").replace("us", "\u00b5s"));
});

test("the running clock's count: one period, one of several, a mean of the rest", () => {
  // A single 79 ns period: "79 ns, one period", not "mean of 1 periods".
  const one = measureReading("period", 0, clockOf([79]), GS)!;
  assert.deepEqual(one, { value: nb("79 ns"), note: "one period" });
  const base = { median: 27, min: 27, max: 27, runs: 1, span: 27 };
  assert.equal(clockNote({ ...base, count: 7, used: 1 }), "one of 7 periods");
  assert.equal(clockNote({ ...base, count: 7, used: 7 }), "mean of 7 periods");
  assert.equal(clockNote({ ...base, count: 32, used: 31 }), "mean of 31 of 32 periods");
});
