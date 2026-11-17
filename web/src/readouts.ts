// Readouts: times, rates and values shown with the digits their source supports and no more.
//
// Arithmetic is exact (BigInt rationals), so a digit count never depends on how a float happened
// to round. A number is shown by rounding it at a "place", a power of ten; the place is chosen
// from the number's resolution, and the unit from the rounded number's magnitude.

import type { MeasureKind } from "./annotations.ts";
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

/** One tick of the recording's: a reading of one source's own rate. */
export const ONE: Ratio = { num: 1, den: 1 };

/**
 * The place of one tick: the largest power of ten not larger than a tick's length. An interval
 * is a whole number of ticks, so its last digit sits here. At 1 GS/s that is 1 ns; at 200 kS/s
 * (5 us) it is 1 us, and every interval is then exact at that place. `step` is the ticks one tick
 * of the reading's source spans, its `m` (FORMAT.md, "Times"): a ratio where a source's tick is
 * not a whole number of the recording's.
 */
export function tickPlace(timebase: Ratio, step: Ratio = ONE): number {
  return placeOf(big(timebase.den) * big(step.num), big(timebase.num) * big(step.den));
}

/**
 * An interval of a whole number of ticks: 1234567 ticks at 1 GS/s is 1.234567 ms. `step` is the
 * ticks one tick of the reading's source spans (`readingStep`), where a merged recording ticks
 * finer than its sources: the last digit sits at the source's tick.
 */
export function formatInterval(ticks: number, timebase: Ratio, step: Ratio = ONE): string {
  const p = tickPlace(timebase, step);
  return formatScaled(roundAt(big(ticks) * big(timebase.den), big(timebase.num), p), p, TIME_UNITS);
}

/**
 * An interval of `ticks` of a source's own ticks, each `m` of the recording's, to the digit of
 * the source's tick: a channel's move or kept span, which its header states in its source's ticks.
 */
export function formatSourceInterval(ticks: number, timebase: Ratio, m: Ratio): string {
  const p = tickPlace(timebase, m);
  return formatScaled(roundAt(big(ticks) * big(m.num) * big(timebase.den), big(m.den) * big(timebase.num), p), p, TIME_UNITS);
}

/**
 * An interval to three significant figures, for a summary line: 927888190 ticks at 1 GS/s is
 * 928 ms. Never finer than one tick.
 */
export function formatBrief(ticks: number, timebase: Ratio, step: Ratio = ONE): string {
  const n = big(Math.abs(ticks)) * big(timebase.den);
  const d = big(timebase.num);
  const tick = tickPlace(timebase, step);
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
export function formatRate(periods: number, span: number, timebase: Ratio, runs = 1, step: Ratio = ONE): string | null {
  if (periods <= 0 || span <= 0 || runs <= 0) return null;
  const n = big(periods) * big(timebase.num);
  const d = big(span) * big(timebase.den);
  // Each run's span is wrong by less than one tick of the source: `step` of these ticks.
  const p = placeOf(n * big(runs) * big(step.num), d * big(span) * big(step.den));
  return formatScaled(roundAt(n, d, p), p, RATE_UNITS);
}

/**
 * The mean of `periods` periods that together span `span` ticks: span / periods ticks. By the rule
 * above the span is wrong by less than `runs` ticks, so the mean is uncertain by runs / periods
 * ticks, and may carry digits finer than one tick.
 */
export function formatMeanPeriod(periods: number, span: number, timebase: Ratio, runs = 1, step: Ratio = ONE): string | null {
  if (periods <= 0 || span <= 0 || runs <= 0) return null;
  const n = big(span) * big(timebase.den);
  const d = big(periods) * big(timebase.num);
  const p = placeOf(big(runs) * big(step.num) * big(timebase.den), d * big(step.den));
  return formatScaled(roundAt(n, d, p), p, TIME_UNITS);
}

/**
 * A value of an analog channel, to the digit of one code step (`resolution`), and to `significant`
 * figures where the channel states them, whichever is coarser. A source that stored floating-point
 * values states no resolution, and gets four significant figures unless it states otherwise.
 */
export function formatValue(value: number, resolution: number | null, unit: string, significant?: number): string {
  const [n, d] = rational(value);
  const figures = significant ?? (resolution !== null && resolution > 0 ? undefined : 4);
  let p = -Infinity;
  if (resolution !== null && resolution > 0) {
    const [rn, rd] = rational(resolution);
    p = placeOf(rn, rd);
  }
  // Zero has no leading digit to count figures from: it keeps to the code step where there is one.
  if (figures !== undefined && (n !== 0n || p === -Infinity)) p = Math.max(p, n === 0n ? 0 : placeOf(n < 0n ? -n : n, d) - (figures - 1));
  return formatScaled(roundAt(n, d, p), p, prefixedUnits(unit));
}

const COUNT = new Intl.NumberFormat("en");

export function formatCount(n: number): string {
  return COUNT.format(n);
}

/** The most distinct periods a span's figures count, each value with how many times it occurs:
 * past it they keep a few numbers, however many periods the span holds. A clock's periods take
 * few values, so this is far past any clock's. */
export const DISTINCT_AT_MOST = 2 ** 21;

/** The most edges a span's periods take from a reader at once, so the memory they hold stays
 * bounded however long the span. */
export const PIECE_EDGES = 2 ** 20;

export interface PeriodStats {
  /** Periods between consecutive rising edges. */
  readonly count: number;
  /** In ticks. The median of an even count is the lower middle, so it is a period that occurred.
   * Null past `DISTINCT_AT_MOST` distinct periods. */
  readonly median: number | null;
  readonly min: number;
  readonly max: number;
  /**
   * The running clock: the periods inside Tukey's fences (ANNOTATIONS.md, "Measures"). The fences
   * keep a clock's jitter and quantisation, and drop pauses and stretched cycles that occur in
   * fewer than a quarter of the periods. `used` periods, in `runs` runs of consecutive periods,
   * summing to `span` ticks. Past `DISTINCT_AT_MOST` distinct periods no fence is set: every
   * period, one run, from the first rising edge to the last.
   */
  readonly used: number;
  readonly runs: number;
  readonly span: number;
}

/** A sorted list of distinct periods and how many times each occurs. */
interface Counts {
  values: Float64Array;
  counts: Float64Array;
  length: number;
}

/** The distinct periods a counter tallies as they come, before any batch: a clock's few. */
const HOT = 8;

/** `periods[0, n)`, sorted in place, as distinct values and their counts. */
function counted(periods: Float64Array, n: number): Counts {
  const sorted = periods.subarray(0, n).sort();
  const values = new Float64Array(n);
  const counts = new Float64Array(n);
  let length = 0;
  for (let i = 0; i < n; i++) {
    if (length > 0 && values[length - 1] === sorted[i]) counts[length - 1]++;
    else {
      values[length] = sorted[i];
      counts[length++] = 1;
    }
  }
  return { values, counts, length };
}

/** Two sorted lists of distinct values and counts, as one. */
function merged(a: Counts, b: Counts): Counts {
  const values = new Float64Array(a.length + b.length);
  const counts = new Float64Array(a.length + b.length);
  let [i, j, length] = [0, 0, 0];
  while (i < a.length || j < b.length) {
    if (j === b.length || (i < a.length && a.values[i] < b.values[j])) {
      values[length] = a.values[i];
      counts[length++] = a.counts[i++];
    } else if (i === a.length || b.values[j] < a.values[i]) {
      values[length] = b.values[j];
      counts[length++] = b.counts[j++];
    } else {
      values[length] = a.values[i];
      counts[length++] = a.counts[i++] + b.counts[j++];
    }
  }
  return { values, counts, length };
}

/**
 * A span's periods, rising edge to rising edge, in two passes over its edges in order, as a
 * reader that walks a span's blocks without joining their edges takes them. The first counts each
 * distinct period, a batch at a time, sorted and merged: from the counts come the median and
 * Tukey's fences at `periodStats`' indices. The second, given the fences (`fences`), finds the
 * periods inside them in order: how many, in how many runs, and their sum, as `periodStats` sums
 * them. Past `distinctAtMost` distinct periods the counts are dropped, no second pass is wanted,
 * and only the count, least, most and ends are kept.
 */
export class PeriodCounter {
  readonly #level: 0 | 1;
  readonly #distinctAtMost: number;
  #rising: boolean;
  #first = NaN;
  #last = NaN;
  #count = 0;
  #min = Infinity;
  #max = -Infinity;
  /** The periods since the last merge, and the counts merged so far: null past the cap. */
  #batch = new Float64Array(1 << 16);
  #held = 0;
  /** Up to `HOT` distinct periods since the last merge, each with its count, kept out of the
   * batch; `#hit` the one the last period was. */
  readonly #hot = new Float64Array(HOT);
  readonly #hotCounts = new Float64Array(HOT);
  #hotLength = 0;
  #hit = 0;
  #counts: Counts | null = { values: new Float64Array(0), counts: new Float64Array(0), length: 0 };
  /** The second pass's state, once `fences` gave the fences. */
  #fence: { lo: number; hi: number; rising: boolean; last: number; inRun: boolean; used: number; runs: number; span: number } | null = null;

  /** `level` is the level before the first edge. */
  constructor(level: 0 | 1, distinctAtMost = DISTINCT_AT_MOST) {
    this.#level = level;
    this.#rising = level === 0;
    this.#distinctAtMost = distinctAtMost;
  }

  /** The first pass's next edge. */
  edge(tick: number): void {
    if (this.#rising) {
      if (Number.isNaN(this.#last)) this.#first = tick;
      else this.#period(tick - this.#last);
      this.#last = tick;
    }
    this.#rising = !this.#rising;
  }

  #period(p: number): void {
    this.#count++;
    if (p < this.#min) this.#min = p;
    if (p > this.#max) this.#max = p;
    if (!this.#counts) return;
    const hot = this.#hot;
    if (this.#hit < this.#hotLength && hot[this.#hit] === p) {
      this.#hotCounts[this.#hit]++;
      return;
    }
    for (let k = 0; k < this.#hotLength; k++) {
      if (hot[k] === p) {
        this.#hotCounts[k]++;
        this.#hit = k;
        return;
      }
    }
    if (this.#hotLength < HOT) {
      hot[this.#hotLength] = p;
      this.#hotCounts[this.#hotLength] = 1;
      this.#hit = this.#hotLength++;
      return;
    }
    if (this.#held === this.#batch.length) this.#merge();
    this.#batch[this.#held++] = p;
  }

  /** The batch and the hot periods merged into the counts; a batch at least as long as the
   * counts after. */
  #merge(): void {
    if (!this.#counts) return;
    const n = this.#hotLength;
    const order = Array.from({ length: n }, (_, k) => k).sort((a, b) => this.#hot[a] - this.#hot[b]);
    const hot = { values: Float64Array.from(order, (k) => this.#hot[k]), counts: Float64Array.from(order, (k) => this.#hotCounts[k]), length: n };
    const all = merged(merged(this.#counts, hot), counted(this.#batch, this.#held));
    [this.#held, this.#hotLength, this.#hit] = [0, 0, 0];
    if (all.length > this.#distinctAtMost) {
      [this.#counts, this.#batch] = [null, new Float64Array(0)];
      return;
    }
    this.#counts = all;
    if (this.#batch.length < all.length) this.#batch = new Float64Array(2 ** Math.ceil(Math.log2(all.length)));
  }

  /** The sorted period at index `i`. */
  #at(i: number): number {
    const { values, counts, length } = this.#counts!;
    let k = 0;
    for (let seen = counts[0]; seen <= i && k + 1 < length; seen += counts[++k]);
    return values[k];
  }

  /**
   * Ends the first pass. The fences, where a second pass over the same edges is wanted: none
   * where the span has fewer than two rising edges, or past `distinctAtMost` distinct periods.
   */
  fences(): { lo: number; hi: number } | null {
    this.#merge();
    const n = this.#count;
    if (n === 0 || !this.#counts) return null;
    // Tukey's fences: Q1 = p[⌊(n − 1)/4⌋], Q3 = p[⌈3(n − 1)/4⌉], IQR at least one tick.
    const q1 = this.#at(Math.floor((n - 1) / 4));
    const q3 = this.#at(Math.ceil((3 * (n - 1)) / 4));
    const iqr = Math.max(q3 - q1, 1);
    const [lo, hi] = [q1 - 1.5 * iqr, q3 + 1.5 * iqr];
    this.#fence = { lo, hi, rising: this.#level === 0, last: NaN, inRun: false, used: 0, runs: 0, span: 0 };
    return { lo, hi };
  }

  /** The second pass's next edge, after `fences`. */
  again(tick: number): void {
    const f = this.#fence!;
    if (f.rising) {
      if (!Number.isNaN(f.last)) {
        const p = tick - f.last;
        if (p >= f.lo && p <= f.hi) {
          f.used++;
          f.span += p;
          if (!f.inRun) f.runs++;
          f.inRun = true;
        } else f.inRun = false;
      }
      f.last = tick;
    }
    f.rising = !f.rising;
  }

  /** The figures, as `periodStats` gives them, once `fences` and any second pass are done. */
  stats(): PeriodStats | null {
    const count = this.#count;
    if (count === 0) return null;
    const [min, max] = [this.#min, this.#max];
    if (!this.#counts || !this.#fence) return { count, median: null, min, max, used: count, runs: 1, span: this.#last - this.#first };
    const { used, runs, span } = this.#fence;
    return { count, median: this.#at((count - 1) >> 1), min, max, used, runs, span };
  }
}

/**
 * Periods measured rising edge to rising edge. `level` is the level before `ticks[0]`. Null when
 * there are fewer than two rising edges. Past `distinctAtMost` distinct periods (`PeriodCounter`)
 * the median is null and no fence is set.
 */
export function periodStats(level: 0 | 1, ticks: ArrayLike<number>, distinctAtMost = DISTINCT_AT_MOST): PeriodStats | null {
  const counter = new PeriodCounter(level, distinctAtMost);
  for (let i = 0; i < ticks.length; i++) counter.edge(ticks[i]);
  if (counter.fences()) for (let i = 0; i < ticks.length; i++) counter.again(ticks[i]);
  return counter.stats();
}

/** The running clock's rate, with its digits. */
export function formatClockRate(s: PeriodStats, timebase: Ratio, step: Ratio = ONE): string {
  return formatRate(s.used, s.span, timebase, s.runs, step)!;
}

/** The running clock's period, with its digits. */
export function formatClockPeriod(s: PeriodStats, timebase: Ratio, step: Ratio = ONE): string {
  return formatMeanPeriod(s.used, s.span, timebase, s.runs, step)!;
}

/**
 * "mean of 31 of 32 periods", "mean of 7 periods" when it kept them all, "one of 7 periods", "one
 * period"; past `DISTINCT_AT_MOST` distinct periods, "mean of 3,000,000 periods: no median, and
 * pauses not set aside, past 2,097,152 distinct periods".
 */
export function clockNote(s: PeriodStats): string {
  if (s.median === null) return `mean of ${formatCount(s.count)} periods: no median, and pauses not set aside, past ${formatCount(DISTINCT_AT_MOST)} distinct periods`;
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

/**
 * A value range's two marks, each a multiple of 10^place (an axis's labels), in the one unit the
 * larger magnitude picks, so both read alike: 0 to 2.0 mA is "2.0 mA" and "0.0 mA".
 */
export function formatMarks(top: number, bottom: number, place: number, unit: string): [string, string] {
  const units = prefixedUnits(unit);
  const [tn, td] = rational(top);
  const [bn, bd] = rational(bottom);
  const t = roundAt(tn, td, place);
  const b = roundAt(bn, bd, place);
  const abs = (x: bigint) => (x < 0n ? -x : x);
  const u = unitFor(abs(t) >= abs(b) ? t : b, place, units);
  return [formatScaled(t, place, units, u), formatScaled(b, place, units, u)];
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
 * last, of either polarity, to the tick of the channel's source: `step` is that tick in axis
 * ticks, its `m` (`readingStep`). Null when the span has too few edges to say anything.
 */
export function measureReading(what: MeasureKind, level: 0 | 1, ticks: ArrayLike<number>, timebase: Ratio, step: Ratio): Reading | null {
  if (what === "duration") {
    if (ticks.length < 2) return null;
    return { value: formatInterval(ticks[ticks.length - 1] - ticks[0], timebase, step), note: "first edge to last" };
  }
  const stats = periodStats(level, ticks);
  if (!stats) return { value: "\u2013", note: "no full period" };
  return { value: what === "rate" ? formatClockRate(stats, timebase, step) : formatClockPeriod(stats, timebase, step), note: clockNote(stats) };
}

/** A download size: 4828 bytes is 4.7 KiB. */
export function formatBytes(n: number): string {
  const units = ["bytes", "KiB", "MiB", "GiB"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return u === 0 ? `${n} bytes` : `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
}
