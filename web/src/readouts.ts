// Readouts: times, rates and values shown with the digits their source supports and no more.
//
// Arithmetic is exact (BigInt rationals), so a digit count never depends on how a float happened
// to round. A number is shown by rounding it at a "place", a power of ten; the place is chosen
// from the number's resolution, and the unit from the rounded number's magnitude.

import type { Ratio } from "./source.ts";
import { lowerBound } from "./search.ts";

export const NBSP = " ";
export const MINUS = "−";

export interface Unit {
  readonly exp: number;
  readonly name: string;
}

export const TIME_UNITS: readonly Unit[] = [
  { exp: 0, name: "s" },
  { exp: -3, name: "ms" },
  { exp: -6, name: "µs" },
  { exp: -9, name: "ns" },
  { exp: -12, name: "ps" },
];

export const RATE_UNITS: readonly Unit[] = [
  { exp: 9, name: "GHz" },
  { exp: 6, name: "MHz" },
  { exp: 3, name: "kHz" },
  { exp: 0, name: "Hz" },
];

/** SI prefixes for a value's unit, largest first. */
export function prefixedUnits(unit: string): Unit[] {
  return [
    { exp: 9, name: `G${unit}` },
    { exp: 6, name: `M${unit}` },
    { exp: 3, name: `k${unit}` },
    { exp: 0, name: unit },
    { exp: -3, name: `m${unit}` },
    { exp: -6, name: `µ${unit}` },
    { exp: -9, name: `n${unit}` },
    { exp: -12, name: `p${unit}` },
  ];
}

function big(x: number): bigint {
  if (!Number.isSafeInteger(x)) throw new RangeError(`${x} is not a safe integer`);
  return BigInt(x);
}

function pow10(e: number): bigint {
  return 10n ** BigInt(e);
}

/** Exactly the decimal a finite number prints as, as a fraction n / d with d > 0. */
export function rational(x: number): [bigint, bigint] {
  const m = /^(-?)(\d+)(?:\.(\d+))?(?:e([+-]?\d+))?$/.exec(String(x));
  if (!m) throw new RangeError(`${x} is not finite`);
  const [, sign, int, frac = "", exp = "0"] = m;
  let n = BigInt(int + frac);
  const e = Number(exp) - frac.length;
  let d = 1n;
  if (e >= 0) n *= pow10(e);
  else d = pow10(-e);
  return [sign ? -n : n, d];
}

/** n/d compared with 10^p: negative, zero or positive. Requires n, d > 0. */
function cmpPow10(n: bigint, d: bigint, p: number): number {
  const [a, b] = p >= 0 ? [n, d * pow10(p)] : [n * pow10(-p), d];
  return a < b ? -1 : a > b ? 1 : 0;
}

/** The exponent of the largest power of ten not larger than n/d, for n/d > 0. */
export function placeOf(n: bigint, d: bigint): number {
  if (n <= 0n || d <= 0n) throw new RangeError("placeOf needs a positive ratio");
  // The difference in digit counts is the place or one above it, never below.
  let p = n.toString().length - d.toString().length;
  if (cmpPow10(n, d, p) < 0) p--;
  return p;
}

/** n / (d * 10^p), rounded to an integer, halves away from zero. Requires d > 0. */
export function roundAt(n: bigint, d: bigint, p: number): bigint {
  const [a, b] = p >= 0 ? [n, d * pow10(p)] : [n * pow10(-p), d];
  const neg = a < 0n;
  const abs = neg ? -a : a;
  const q = (2n * abs + b) / (2n * b);
  return neg ? -q : q;
}

function digitsAt(scaled: bigint, p: number, unit: Unit): string {
  const neg = scaled < 0n;
  let s = (neg ? -scaled : scaled).toString();
  if (p >= unit.exp) {
    if (s !== "0") s += "0".repeat(p - unit.exp);
  } else {
    const decimals = unit.exp - p;
    s = s.padStart(decimals + 1, "0");
    s = `${s.slice(0, -decimals)}.${s.slice(-decimals)}`;
  }
  return `${neg ? MINUS : ""}${s}${NBSP}${unit.name}`;
}

/** The unit for `scaled` × 10^p: the largest not above its magnitude. */
export function unitFor(scaled: bigint, p: number, units: readonly Unit[]): Unit {
  const abs = scaled < 0n ? -scaled : scaled;
  if (abs === 0n) return units.find((u) => u.exp <= p) ?? units[units.length - 1];
  for (const u of units) {
    const step = u.exp - p;
    if (step <= 0 || abs >= pow10(step)) return u;
  }
  return units[units.length - 1];
}

/** `scaled` × 10^p, in `unit` or the unit its magnitude picks. */
export function formatScaled(scaled: bigint, p: number, units: readonly Unit[], unit?: Unit): string {
  return digitsAt(scaled, p, unit ?? unitFor(scaled, p, units));
}

/**
 * The place of one tick: the largest power of ten not larger than a tick's length. An interval
 * is a whole number of ticks, so its last digit sits here. At 1 GS/s that is 1 ns; at 200 kS/s
 * (5 us) it is 1 us, and every interval is then exact at that place.
 */
export function tickPlace(timebase: Ratio): number {
  return placeOf(big(timebase.den), big(timebase.num));
}

/** An interval of a whole number of ticks: 1234567 ticks at 1 GS/s is 1.234567 ms. */
export function formatInterval(ticks: number, timebase: Ratio): string {
  const p = tickPlace(timebase);
  return formatScaled(roundAt(big(ticks) * big(timebase.den), big(timebase.num), p), p, TIME_UNITS);
}

/** A time from the capture's start, which is an interval from tick 0. */
export const formatTime = formatInterval;

/**
 * An interval to three significant figures, for a summary line: 927888190 ticks at 1 GS/s is
 * 928 ms. Never finer than one tick.
 */
export function formatBrief(ticks: number, timebase: Ratio): string {
  const n = big(Math.abs(ticks)) * big(timebase.den);
  const d = big(timebase.num);
  const tick = tickPlace(timebase);
  if (n === 0n) return formatScaled(0n, tick, TIME_UNITS);
  let p = Math.max(placeOf(n, d) - 2, tick);
  let scaled = roundAt(n, d, p);
  // Rounding up can add a digit (999.6 ms to 1000 ms): one place coarser then.
  if (scaled >= 1000n) scaled = roundAt(n, d, ++p);
  return formatScaled(ticks < 0 ? -scaled : scaled, p, TIME_UNITS);
}

/**
 * The mean rate over `periods` periods that together span `span` ticks, in Hz with an SI prefix.
 *
 * The digits: each edge is recorded at the first sample after it, so an edge's tick is late by
 * less than one tick, and a span between two edges is wrong by less than one tick. Over `runs`
 * separate runs of periods the total is wrong by less than `runs` ticks, so the rate
 * f = periods / span is uncertain by about f × runs / span. The last digit shown is at the largest
 * power of ten not larger than that uncertainty. A single period of 40 ns is 25 MHz give or take
 * 0.6 MHz, so it shows 25.0 MHz; the mean over 1000 such periods in one run shows 25.0000 MHz.
 */
export function formatRate(periods: number, span: number, timebase: Ratio, runs = 1): string | null {
  if (periods <= 0 || span <= 0 || runs <= 0) return null;
  const n = big(periods) * big(timebase.num);
  const d = big(span) * big(timebase.den);
  const p = placeOf(n * big(runs), d * big(span));
  return formatScaled(roundAt(n, d, p), p, RATE_UNITS);
}

/**
 * The mean of `periods` periods that together span `span` ticks: span / periods ticks. By the rule
 * above the span is wrong by less than `runs` ticks, so the mean is uncertain by runs / periods
 * ticks, and may carry digits finer than one tick.
 */
export function formatMeanPeriod(periods: number, span: number, timebase: Ratio, runs = 1): string | null {
  if (periods <= 0 || span <= 0 || runs <= 0) return null;
  const n = big(span) * big(timebase.den);
  const d = big(periods) * big(timebase.num);
  const p = placeOf(big(runs) * big(timebase.den), d);
  return formatScaled(roundAt(n, d, p), p, TIME_UNITS);
}

/**
 * A value of an analog channel, to the digit of one code step (`resolution`). A source that stored
 * floating-point values states no resolution, and gets four significant digits.
 */
export function formatValue(value: number, resolution: number | null, unit: string): string {
  const [n, d] = rational(value);
  let p: number;
  if (resolution !== null && resolution > 0) {
    const [rn, rd] = rational(resolution);
    p = placeOf(rn, rd);
  } else {
    p = n === 0n ? 0 : placeOf(n < 0n ? -n : n, d) - 3;
  }
  return formatScaled(roundAt(n, d, p), p, prefixedUnits(unit));
}

const COUNT = new Intl.NumberFormat("en");

export function formatCount(n: number): string {
  return COUNT.format(n);
}

export interface PeriodStats {
  /** Periods between consecutive rising edges. */
  readonly count: number;
  /** In ticks. The median of an even count is the lower middle, so it is a period that occurred. */
  readonly median: number;
  readonly min: number;
  readonly max: number;
  /**
   * The running clock: the periods inside Tukey's fences (ANNOTATIONS.md, "Measures"). The fences
   * keep a clock's jitter and quantisation, and drop pauses and stretched cycles that occur in
   * fewer than a quarter of the periods. `used` periods, in `runs` runs of consecutive periods,
   * summing to `span` ticks.
   */
  readonly used: number;
  readonly runs: number;
  readonly span: number;
}

/**
 * Periods measured rising edge to rising edge. `level` is the level before `ticks[0]`. Null when
 * there are fewer than two rising edges.
 */
export function periodStats(level: 0 | 1, ticks: ArrayLike<number>): PeriodStats | null {
  // Edge i is rising when the level before it is low; the level before edge i is level ^ (i & 1).
  const first = level === 0 ? 0 : 1;
  const count = Math.max(0, Math.floor((ticks.length - first - 1) / 2));
  if (count === 0) return null;
  const periods = new Float64Array(count);
  for (let k = 0; k < count; k++) periods[k] = ticks[first + 2 * k + 2] - ticks[first + 2 * k];
  const sorted = periods.slice().sort();
  const median = sorted[(count - 1) >> 1];
  // Tukey's fences: Q1 = p[⌊(n − 1)/4⌋], Q3 = p[⌈3(n − 1)/4⌉], IQR at least one tick.
  const q1 = sorted[Math.floor((count - 1) / 4)];
  const q3 = sorted[Math.ceil((3 * (count - 1)) / 4)];
  const iqr = Math.max(q3 - q1, 1);
  const [lo, hi] = [q1 - 1.5 * iqr, q3 + 1.5 * iqr];
  let used = 0;
  let runs = 0;
  let span = 0;
  let inRun = false;
  for (const p of periods) {
    if (p >= lo && p <= hi) {
      used++;
      span += p;
      if (!inRun) runs++;
      inRun = true;
    } else inRun = false;
  }
  return { count, median, min: sorted[0], max: sorted[count - 1], used, runs, span };
}

/** The running clock's rate, with its digits. */
export function formatClockRate(s: PeriodStats, timebase: Ratio): string {
  return formatRate(s.used, s.span, timebase, s.runs)!;
}

/** The running clock's period, with its digits. */
export function formatClockPeriod(s: PeriodStats, timebase: Ratio): string {
  return formatMeanPeriod(s.used, s.span, timebase, s.runs)!;
}

/** "mean of 31 of 32 periods", "mean of 7 periods" when it kept them all, "one of 7 periods", "one period". */
export function clockNote(s: PeriodStats): string {
  if (s.used === 1) return s.count === 1 ? "one period" : `one of ${formatCount(s.count)} periods`;
  return s.used === s.count ? `mean of ${formatCount(s.count)} periods` : `mean of ${formatCount(s.used)} of ${formatCount(s.count)} periods`;
}

/** Edges in [start, end) of an ascending tick array. */
export function countEdges(ticks: ArrayLike<number>, start: number, end: number): number {
  return Math.max(0, lowerBound(ticks, end) - lowerBound(ticks, start));
}

/** A value known to be a multiple of 10^place (an axis label), in `unit` with an SI prefix. */
export function formatAt(value: number, place: number, unit: string): string {
  const [n, d] = rational(value);
  return formatScaled(roundAt(n, d, place), place, prefixedUnits(unit));
}

/** 1, 2 or 5 times a power of ten, the smallest not below x > 0, and that power's exponent. */
export function niceStep(x: number): { step: number; place: number } {
  let place = Math.floor(Math.log10(x));
  for (;;) {
    for (const m of [1, 2, 5]) {
      const step = Number(`${m}e${place}`);
      if (step >= x * (1 - 1e-12)) return { step, place };
    }
    place++;
  }
}

export interface Reading {
  readonly value: string;
  readonly note: string;
}

/**
 * A `measure` reading (ANNOTATIONS.md, "Measures") over a span's edges: `ticks` are the edges at
 * ticks in [start, end), as `Source.edges` returns them, and `level` the level before the first.
 * `rate` and `period` read the running clock; `duration` the time from the first edge to the
 * last, of either polarity, to the tick. Null when the span has too few edges to say anything;
 * undefined for a measure it does not know.
 */
export function measureReading(what: string, level: 0 | 1, ticks: ArrayLike<number>, timebase: Ratio): Reading | null | undefined {
  if (what === "duration") {
    if (ticks.length < 2) return null;
    return { value: formatInterval(ticks[ticks.length - 1] - ticks[0], timebase), note: "first edge to last" };
  }
  if (what !== "rate" && what !== "period") return undefined;
  const stats = periodStats(level, ticks);
  if (!stats) return { value: "\u2013", note: "no full period" };
  return { value: what === "rate" ? formatClockRate(stats, timebase) : formatClockPeriod(stats, timebase), note: clockNote(stats) };
}
