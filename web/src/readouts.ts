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
 * The mean rate over `periods` periods spanning `span` ticks, in Hz with an SI prefix.
 *
 * The digits: each edge is recorded at the first sample after it, so an edge's tick is late by
 * less than one tick, and a span between two edges is wrong by less than one tick. The rate
 * f = periods / span is then uncertain by about f / span (span in ticks). The last digit shown is
 * at the largest power of ten not larger than that uncertainty. A single period of 40 ns is
 * 25 MHz give or take 0.6 MHz, so it shows 25.0 MHz; the mean over 1000 such periods shows
 * 25.0000 MHz.
 */
export function formatRate(periods: number, span: number, timebase: Ratio): string | null {
  if (periods <= 0 || span <= 0) return null;
  const n = big(periods) * big(timebase.num);
  const d = big(span) * big(timebase.den);
  const p = placeOf(n, d * big(span));
  return formatScaled(roundAt(n, d, p), p, RATE_UNITS);
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
  /** Ticks from the first rising edge to the last. */
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
  periods.sort();
  return {
    count,
    median: periods[(count - 1) >> 1],
    min: periods[0],
    max: periods[count - 1],
    span: ticks[first + 2 * count] - ticks[first],
  };
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
