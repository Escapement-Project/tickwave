// The renderer's only view of a recording. Every method is async: blocks arrive from a worker and
// the network, and the renderer draws what it has and redraws when more arrives.
//
// Channels are named by their index in `RecordingInfo.channels`. Ticks are integers below 2^53,
// counted from the capture's start; ranges are half-open, [start, end). Every tick a method takes
// or gives is on the recording's axis. An analog channel states its samples' places in its
// source's own ticks, which its `map` places on the axis (FORMAT.md, "Times").

import type { PeriodStats } from "./readouts.ts";

/** A positive rational number, num / den. */
export interface Ratio {
  readonly num: number;
  readonly den: number;
}

/**
 * Where a channel's source ticks lie on the recording's axis (FORMAT.md, "Times"): source tick `k`
 * at `origin + floor((k + offset) × m)`, `m` how many axis ticks one source tick lasts.
 */
export interface Mapping {
  readonly origin: number;
  readonly offset: number;
  readonly m: Ratio;
}

/** Source tick `k` at axis tick `k`. */
export const IDENTITY: Mapping = { origin: 0, offset: 0, m: { num: 1, den: 1 } };

/** `floor(x × a / b)`, exact for every term below 2^53; a and b positive. */
export function mulDivFloor(x: number, a: number, b: number): number {
  // A quotient of two integers below 2^53 floors exactly: its rounding error is below 1/b, the
  // least distance to the next integer.
  const product = x * a;
  if (Number.isSafeInteger(product)) return Math.floor(product / b);
  // x = q b + r with 0 ≤ r < b, so x × a / b = a q + a r / b, and a r < a b. Exact in plain
  // numbers wherever a × b, and the result, stay below 2^53, as every real m's terms do.
  if (a * b <= Number.MAX_SAFE_INTEGER && Number.isSafeInteger(x) && Math.abs(x) + b <= Number.MAX_SAFE_INTEGER) {
    const q = Math.floor(x / b);
    const high = a * q;
    const result = high + Math.floor((a * (x - q * b)) / b);
    if (Number.isSafeInteger(high) && Number.isSafeInteger(result)) return result;
  }
  const [n, d] = [BigInt(x) * BigInt(a), BigInt(b)];
  const q = n / d;
  return Number(n % d !== 0n && n < 0n ? q - 1n : q);
}

/** The axis tick source tick `k` maps to. */
export function axisTick(map: Mapping, k: number): number {
  return map.origin + mulDivFloor(k + map.offset, map.m.num, map.m.den);
}

/** The least source tick that maps to axis tick `t` or later. */
export function firstAt(map: Mapping, t: number): number {
  // ceil((t − origin) × den / num) − offset.
  return -mulDivFloor(-(t - map.origin), map.m.den, map.m.num) - map.offset;
}

export interface DigitalChannel {
  readonly kind: "digital";
  readonly name: string;
  /** The source's own name for the channel, as the header's `source_name` gives it. */
  readonly sourceName?: string;
  /** The first tick whose level is known, where the channel's data starts after the capture's
   * does. Absent: 0. Nothing is drawn before it. */
  readonly start?: number;
  /** The first tick past the channel's data. Nothing is drawn at or after it. */
  readonly end: number;
  /** Spans after `start` where the level was not known, in order (FORMAT.md, "Gaps"). */
  readonly gaps?: readonly Gap[];
  /**
   * The gaps are in the file's list pages, read only for the spans a view reads: `gaps` is
   * absent, `start` is where the channel starts before any gap from there, and each answer for
   * the channel carries the gaps its span needs (`Listed`).
   */
  readonly paged?: boolean;
  /** Where its source's ticks lie on the axis, which places its stated `kept` span; absent, at
   * their own ticks. Its other ticks are on the axis already. */
  readonly map?: Mapping;
}

/** Ticks `from..to` where a digital channel's level was not known; from `to` on it is `level`. */
export interface Gap {
  readonly from: number;
  readonly to: number;
  readonly level: 0 | 1;
}

/**
 * Sample `index` starts a new run at `tick` plus `phase` of its channel's grid's `1/d` ticks
 * (`gridOf`), after `lost` samples the instrument lost: 0 when it paused (FORMAT.md, "Gaps").
 */
export interface Break {
  readonly index: number;
  readonly tick: number;
  readonly lost: number;
  /** In the channel's grid's `1/d` ticks; absent for 0. */
  readonly phase?: number;
}

export interface AnalogChannel {
  readonly kind: "analog";
  readonly name: string;
  /** The source's own name for the channel, as the header's `source_name` gives it. */
  readonly sourceName?: string;
  /** On the axis: where the last sample's time ends. */
  readonly end: number;
  /** The unit of a value, such as "V" or "A". */
  readonly unit: string;
  /** The source tick the first run starts at. Sample i's place is first + phase + i × period, or
   * after a break, the break's tick plus its phase plus (i − index) × period, in the source's
   * ticks, exactly; it lies on the axis at the tick that place maps to (FORMAT.md, "Times"). */
  readonly first: number;
  /** Source ticks between samples. */
  readonly period: Ratio;
  /** How far past `first` the first sample sits, below a tick; absent for 0. */
  readonly phase?: Ratio;
  readonly count: number;
  /** Where the samples start again after a stop, in order, in the source's ticks. */
  readonly breaks?: readonly Break[];
  /**
   * The breaks are in the file's list pages, read only for the spans a view reads: `breaks` is
   * absent, `end` is the recording's length, and each answer for the channel carries the breaks
   * its span needs (`Listed`).
   */
  readonly paged?: boolean;
  /** Samples the instrument lost before the first and after the last, where it lost any. */
  readonly lostBefore?: number;
  readonly lostAfter?: number;
  /** Where the source's ticks lie on the axis; absent, at their own ticks. */
  readonly map?: Mapping;
  /**
   * The value one code step stands for, where the source recorded integer codes; readouts stop at
   * that digit. Null for a source that stored floating-point values.
   */
  readonly resolution: number | null;
  /** At most this many significant figures in a reading, where the channel states it: an instrument
   * whose step changes with its range. */
  readonly significant?: number;
  /** The smallest and largest value in the recording. */
  readonly min: number;
  readonly max: number;
  /** A derived channel: computed by `op` from its `operands`' values, named in order (FORMAT.md,
   * "Derived channels"). The file stores none of its samples, and it sits where they do. */
  readonly derived?: { readonly op: string; readonly operands: readonly string[] };
}

/**
 * A channel this version can't read (FORMAT.md, "Unavailable channels"): it keeps its place, so
 * every other channel keeps its number, and its name, and nothing of its data is read.
 */
export interface UnavailableChannel {
  readonly kind: "unavailable";
  readonly name: string;
  /** The source's own name for the channel, as the header's `source_name` gives it. */
  readonly sourceName?: string;
  /** Why this version can't read it. */
  readonly why: string;
  /** The recording's length: nothing of it is drawn. */
  readonly end: number;
}

/** What a channel moved or trimmed when it was converted states (FORMAT.md, "Moved and trimmed
 * channels"), in its source's ticks. */
export interface Shaped {
  /** Source ticks it was moved by, later when positive, and why: `"delay"` is a known delay in
   * its path. */
  readonly offset?: { readonly ticks: number; readonly kind: string };
  /** The source ticks `from..to` where it holds what the source recorded; outside them it was
   * cut. */
  readonly kept?: { readonly from: number; readonly to: number };
}

/** Which of the recording's sources a channel came from: its index in `RecordingInfo.sources`. */
export interface Sourced {
  readonly source?: number;
}

export type Channel = (DigitalChannel | AnalogChannel | UnavailableChannel) & Shaped & Sourced;

/** An analog channel's mapping to the axis. */
export function mapOf(c: Pick<AnalogChannel, "map">): Mapping {
  return c.map ?? IDENTITY;
}

/** Axis ticks between an analog channel's samples, as a float: for a choice of drawing, not a
 * place. */
export function axisPeriod(c: Pick<AnalogChannel, "period" | "map">): number {
  const m = mapOf(c).m;
  return (c.period.num * m.num) / (c.period.den * m.den);
}

/** The first tick the channel can hold data at: a digital channel's `start` (0 when absent), an
 * analog channel's first sample; 0 for one this version can't read. */
export function channelStart(c: Channel): number {
  if (c.kind === "analog") return runStartTick(c, -1);
  return c.kind === "digital" ? (c.start ?? 0) : 0;
}

/** Whether `tick` lies in the channel's data: ticks `channelStart(c)..end`, outside its gaps. */
export function inChannel(c: Channel, tick: number): boolean {
  return c.kind !== "unavailable" && tick >= channelStart(c) && tick < c.end && (c.kind !== "digital" || gapAt(c, tick) === null);
}

/** The gap holding `tick`, or null. */
export function gapAt(c: DigitalChannel, tick: number): Gap | null {
  for (const g of c.gaps ?? []) {
    if (tick < g.from) break;
    if (tick < g.to) return g;
  }
  return null;
}

/**
 * The ticks `[from, to)` of a channel's data, clipped to `[start, end)`: a digital channel's split
 * around its gaps, an analog channel's into its runs, each from its first sample to the place after
 * its last on its grid, or to the next run's start if that comes sooner.
 */
export function knownSpans(c: Channel, start: number, end: number): [number, number][] {
  const spans: [number, number][] = [];
  if (c.kind === "unavailable") return spans;
  const stop = Math.min(end, c.end);
  const keep = (a: number, b: number) => {
    const [s, e] = [Math.max(a, start, channelStart(c)), Math.min(b, stop)];
    if (e > s) spans.push([s, e]);
  };
  if (c.kind === "analog") {
    const runs = c.breaks ?? [];
    for (let k = 0; k <= runs.length; k++) {
      const from = runStartTick(c, k - 1);
      const last = (k < runs.length ? runs[k].index : c.count) - 1;
      const to = k < runs.length ? Math.min(runStartTick(c, k), placeAfter(c, last)) : c.end;
      keep(from, to);
    }
    return spans;
  }
  let at = -Infinity;
  for (const g of c.gaps ?? []) {
    keep(at, g.from);
    at = g.to;
  }
  keep(at, Infinity);
  return spans;
}

/**
 * Edges read from `start`, as they stand past the last gap that ends after `start` and at or before
 * `from`: no edge lies in a gap, and its end sets the level. Unchanged where no gap ends there.
 */
export function rebased(c: Channel, start: number, e: Edges, from: number): { start: number; edges: Edges } {
  let gap: Gap | null = null;
  for (const g of c.kind === "digital" ? (c.gaps ?? []) : []) if (g.to > start && g.to <= from) gap = g;
  if (!gap) return { start, edges: e };
  let lo = 0;
  while (lo < e.ticks.length && e.ticks[lo] < gap.to) lo++;
  return { start: gap.to, edges: { level: gap.level, ticks: e.ticks.subarray(lo) } };
}

// Where an analog channel's samples sit: crates/view/src/runs.rs, which the Rust view reads. A
// sample's place is a whole number of its grid's `1/d` source ticks, and lies on the axis at the
// tick it maps to (FORMAT.md, "Times"). Places are integers held exactly: a number while it is
// one, a BigInt past 2^53.

/** What places an analog channel's samples: its first run's tick, its period and phase, its
 * count and breaks, and its mapping to the axis. */
export type Runs = Pick<AnalogChannel, "first" | "period" | "phase" | "count" | "breaks" | "map">;

/**
 * An analog channel's grid (FORMAT.md, "Header"): its places are whole numbers of `1/d` source
 * ticks, `d` the least common multiple of its period's and its phase's denominators; `step` is its
 * period and `phase` its declared phase, in those.
 */
export interface Grid {
  readonly d: number;
  readonly step: number;
  readonly phase: number;
}

function gcd(a: number, b: number): number {
  while (b !== 0) [a, b] = [b, a % b];
  return a;
}

/** Each channel's grid, once worked out: a channel's period and phase never change. */
const grids = new WeakMap<Pick<AnalogChannel, "period" | "phase">, Grid>();

/** Channel `c`'s grid, from its period and declared phase. */
export function gridOf(c: Pick<AnalogChannel, "period" | "phase">): Grid {
  let held = grids.get(c);
  if (!held) grids.set(c, (held = gridFrom(c)));
  return held;
}

function gridFrom(c: Pick<AnalogChannel, "period" | "phase">): Grid {
  const g = gcd(c.period.num, c.period.den);
  const [pn, pd] = [c.period.num / g, c.period.den / g];
  const f = c.phase ?? { num: 0, den: 1 };
  const h = gcd(f.num, f.den) || 1;
  const [fn, fd] = [f.num / h, f.den / h];
  const d = (pd / gcd(pd, fd)) * fd;
  return { d, step: pn * (d / pd), phase: fn * (d / fd) };
}

type Int = number | bigint;

const big = (x: Int): bigint => (typeof x === "bigint" ? x : BigInt(x));

function add(a: Int, b: Int): Int {
  if (typeof a === "number" && typeof b === "number" && Number.isSafeInteger(a + b)) return a + b;
  return big(a) + big(b);
}

function mul(a: Int, b: Int): Int {
  if (typeof a === "number" && typeof b === "number" && Number.isSafeInteger(a * b)) return a * b;
  return big(a) * big(b);
}

const less = (a: Int, b: Int): boolean => (typeof a === "number" && typeof b === "number" ? a < b : big(a) < big(b));

/** A run: the first, whose phase is the declared one, or one a break starts. */
interface Run {
  readonly index: number;
  readonly tick: number;
  readonly phase: number;
}

const firstRun = (c: Pick<AnalogChannel, "first">, g: Grid): Run => ({ index: 0, tick: c.first, phase: g.phase });
const breakRun = (b: Break): Run => ({ index: b.index, tick: b.tick, phase: b.phase ?? 0 });

/** Where `run` starts, in `1/d` ticks. */
const startOf = (g: Grid, run: Run): Int => add(mul(run.tick, g.d), run.phase);

/** The place of sample `i` on the grid of `run`, in `1/d` ticks; an index before the run's counts
 * back from it. */
const placeOf = (g: Grid, run: Run, i: number): Int => add(startOf(g, run), mul(i - run.index, g.step));

/** The axis tick a place of `units / d` source ticks maps to: `origin + floor((units + offset ×
 * d) × num / (den × d))` (FORMAT.md, "Times"). */
export function axisPlace(map: Mapping, units: Int, d: number): number {
  if (d === 1 && typeof units === "number") return axisTick(map, units);
  const x = add(units, mul(map.offset, d));
  const q = map.m.den * d;
  if (typeof x === "number" && Number.isSafeInteger(q)) return map.origin + mulDivFloor(x, map.m.num, q);
  const n = big(x) * BigInt(map.m.num);
  const qq = BigInt(map.m.den) * BigInt(d);
  const t = n / qq;
  return map.origin + Number(n % qq !== 0n && n < 0n ? t - 1n : t);
}

/** The run that holds sample `i`: the last that starts at or before it. */
function runOf(c: Runs, g: Grid, i: number): Run {
  const b = c.breaks ?? [];
  let lo = 0;
  let hi = b.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (b[mid].index <= i) lo = mid + 1;
    else hi = mid;
  }
  return lo === 0 ? firstRun(c, g) : breakRun(b[lo - 1]);
}

/** The axis tick run `k` starts at: break `k`'s, or the first run's for -1. */
function runStartTick(c: Runs, k: number): number {
  const g = gridOf(c);
  return axisPlace(mapOf(c), startOf(g, k < 0 ? firstRun(c, g) : breakRun(c.breaks![k])), g.d);
}

/** The axis tick of sample `i` of an analog channel, its breaks included. */
export function channelSampleTick(c: Runs, i: number): number {
  const g = gridOf(c);
  return axisPlace(mapOf(c), placeOf(g, runOf(c, g, i), i), g.d);
}

/**
 * `channelSampleTick` for many samples of one channel, as a drawing or a search asks for them: the
 * channel's grid and mapping, and the run of the sample asked for last, are held, so a sample in
 * that run costs a multiply-add and the mapping.
 */
export function sampleTicks(c: Runs): (i: number) => number {
  const g = gridOf(c);
  const map = mapOf(c);
  const { d, step } = g;
  // `axisPlace`'s plain-number path, its terms held: `origin + floor((units + offset × d) × num /
  // (den × d))`, each product and sum still below 2^53, as its checks have them.
  const [origin, shift, num, q] = [map.origin, map.offset * d, map.m.num, map.m.den * d];
  const plain = Number.isSafeInteger(shift) && Number.isSafeInteger(q);
  const b = c.breaks ?? [];
  // The run held: the indices it places, `from` to `to`, its first index and its start.
  let [from, to, index, start] = [0, 0, 0, 0 as Int];
  return (i) => {
    if (!(i >= from && i < to)) {
      let k = 0;
      for (let hi = b.length; k < hi; ) {
        const mid = (k + hi) >>> 1;
        if (b[mid].index <= i) k = mid + 1;
        else hi = mid;
      }
      // The first run places every index before the first break, as `runOf` has it.
      const run = k === 0 ? firstRun(c, g) : breakRun(b[k - 1]);
      [from, to, index, start] = [k === 0 ? -Infinity : run.index, k < b.length ? b[k].index : Infinity, run.index, startOf(g, run)];
    }
    const along = (i - index) * step;
    if (plain && typeof start === "number" && Number.isSafeInteger(along)) {
      const units = start + along;
      const x = units + shift;
      const product = x * num;
      if (Number.isSafeInteger(units) && Number.isSafeInteger(x) && Number.isSafeInteger(product)) return origin + Math.floor(product / q);
    }
    return axisPlace(map, add(start, mul(i - index, step)), d);
  };
}

/** The axis tick of the place after sample `i` on its run's grid: where its time ends, unless the
 * next run or the length comes sooner (FORMAT.md, "Gaps"). */
export function placeAfter(c: Runs, i: number): number {
  const g = gridOf(c);
  return axisPlace(mapOf(c), placeOf(g, runOf(c, g, i), i + 1), g.d);
}

/** The least of `count` indices from 0 at which `past` holds, `count` where it never does; `past`
 * holds from some index on. */
function firstIndex(count: number, past: (i: number) => boolean): number {
  let lo = 0;
  let hi = count;
  while (lo < hi) {
    const mid = lo + Math.floor((hi - lo) / 2);
    if (past(mid)) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

/**
 * The first sample whose axis tick reaches `tick` (passes it, where `strict`), `count` where none
 * does. A sample's axis tick never falls as its index grows, since each run starts after the one
 * before it ends: so it lies in the last run whose first sample doesn't reach `tick`, or is the
 * first sample of the run after it.
 */
function firstPast(c: Runs, tick: number, strict: boolean): number {
  const reaches = (t: number) => (strict ? t > tick : t >= tick);
  const g = gridOf(c);
  const map = mapOf(c);
  const b = c.breaks ?? [];
  const at = (u: Int) => axisPlace(map, u, g.d);
  const k = firstIndex(b.length, (j) => reaches(at(startOf(g, breakRun(b[j])))));
  const run = k === 0 ? firstRun(c, g) : breakRun(b[k - 1]);
  const from = run.index;
  const to = k < b.length ? b[k].index : c.count;
  if (from >= to) return from;
  const past = (i: number) => reaches(at(placeOf(g, run, i)));
  // An estimate from the mapping's inverse, settled against the samples' own ticks: off by a
  // sample or two where a double resolves the ticks, and finished by a bisection where it doesn't.
  const source = ((tick - map.origin) * map.m.den) / map.m.num - map.offset;
  const units = source * g.d - Number(startOf(g, run));
  const estimate = run.index + units / g.step;
  let i = Number.isNaN(estimate) || estimate <= from ? from : estimate >= to ? to : Math.ceil(estimate);
  for (let n = 0; n < 8; n++) {
    if (i > from && past(i - 1)) i--;
    else if (i < to && !past(i)) i++;
    else return i;
  }
  if (i > from && past(i - 1)) return from + firstIndex(i - from, (j) => past(from + j));
  return i + firstIndex(to - i, (j) => past(i + j));
}

/** The index of the first sample at or after axis tick `tick`, breaks included, clamped to
 * `0..=count`. A NaN tick is past every sample. */
export function channelIndexAtOrAfter(c: Runs, tick: number): number {
  if (Number.isNaN(tick)) return c.count;
  return firstPast(c, tick, false);
}

/**
 * `channelIndexAtOrAfter` for a run of ticks that never falls, as a grid's columns give them. Each
 * answer carries on from the one before: it guesses as many samples on as the last step took,
 * steps 1, 2, 4, ... samples on or back from the guess until it passes the answer, and bisects
 * that step. Columns of an even width take a tick or three each, however many samples each holds.
 * A tick lower than the one before starts again from the first sample.
 */
export function indicesAtOrAfter(c: Runs): (tick: number) => number {
  const count = c.count;
  const tickOf = sampleTicks(c);
  // Every sample before `last` lies before `prev`, the tick asked for last; `ahead` is how far the
  // answer before it moved on.
  let [last, ahead, prev] = [0, 0, -Infinity];
  return (tick) => {
    if (Number.isNaN(tick)) return count;
    if (tick < prev) [last, ahead] = [0, 0];
    prev = tick;
    if (last >= count || tickOf(last) >= tick) return last;
    // The answer is past `last`: in `lo..=hi`, where sample `lo - 1` lies before `tick` and `hi`
    // reaches it, or is `count`.
    const guess = Math.min(count, last + Math.max(1, ahead));
    let [lo, hi] = [last + 1, guess];
    if (guess < count && tickOf(guess) < tick) {
      lo = guess + 1;
      hi = lo;
      for (let step = 1; hi < count && tickOf(hi) < tick; step *= 2) [lo, hi] = [hi + 1, hi + step];
      hi = Math.min(hi, count);
    } else {
      let step = 1;
      for (; hi - step > last && tickOf(hi - step) >= tick; step *= 2) hi -= step;
      lo = Math.max(last + 1, hi - step + 1);
    }
    while (lo < hi) {
      const mid = lo + Math.floor((hi - lo) / 2);
      if (tickOf(mid) >= tick) hi = mid;
      else lo = mid + 1;
    }
    [ahead, last] = [lo - last, lo];
    return lo;
  };
}

/** The span each break's lost samples cover, as FORMAT.md words it: from place `index` to place
 * `index + lost` on the grid of the run before the break, ending at the break's start if that
 * comes sooner. Then those lost before the first sample, on the first run's grid up to it, and
 * after the last, on the last run's grid from the place after it. Whole ticks. */
export function lostSpans(c: AnalogChannel): { from: number; to: number; lost: number }[] {
  const runs = c.breaks ?? [];
  const g = gridOf(c);
  const map = mapOf(c);
  const out = runs.flatMap((b, k) => {
    if (b.lost === 0) return [];
    const run = k === 0 ? firstRun(c, g) : breakRun(runs[k - 1]);
    const end = startOf(g, breakRun(b));
    const place = (i: number) => {
      const p = placeOf(g, run, i);
      return axisPlace(map, less(p, end) ? p : end, g.d);
    };
    return [{ from: place(b.index), to: place(b.index + b.lost), lost: b.lost }];
  });
  return [...out, ...endLosses(c)];
}

/** The spans of the samples lost before the first and after the last (FORMAT.md, "Gaps"): before,
 * from the place of `-lostBefore` on the first run's grid, where that is not before tick 0, to the
 * first sample; after, from the place after the last sample to the place of `count + lostAfter`
 * on the last run's grid. A channel whose breaks are in pages knows its last run, and the span
 * after it, only from an answer that reaches its end (`spanned`): without one, the span after is
 * left out. */
export function endLosses(c: AnalogChannel): { from: number; to: number; lost: number }[] {
  const out: { from: number; to: number; lost: number }[] = [];
  const g = gridOf(c);
  const map = mapOf(c);
  if (c.lostBefore) {
    const from = placeOf(g, firstRun(c, g), -c.lostBefore);
    if (!less(from, 0)) out.push({ from: axisPlace(map, from, g.d), to: runStartTick(c, -1), lost: c.lostBefore });
  }
  if (c.lostAfter && c.count > 0 && !(c.paged && c.breaks === undefined)) {
    const runs = c.breaks ?? [];
    const run = runs.length === 0 ? firstRun(c, g) : breakRun(runs[runs.length - 1]);
    out.push({ from: placeAfter(c, c.count - 1), to: axisPlace(map, placeOf(g, run, c.count + c.lostAfter), g.d), lost: c.lostAfter });
  }
  return out;
}

/** The ticks a channel spans in a view: its data, and the samples an analog channel lost before
 * its first and after its last, which a view shows as lost. */
export function channelExtent(c: Channel): { start: number; end: number } {
  let start = channelStart(c);
  let end = c.end;
  if (c.kind === "analog") {
    for (const l of endLosses(c)) {
      start = Math.min(start, l.from);
      end = Math.max(end, l.to);
    }
  }
  return { start, end };
}

/**
 * What an answer for a span of a channel whose gaps or breaks are in pages (`paged`) carries: those
 * the span needs. A digital channel's gaps on the axis, as `DigitalChannel.gaps` holds them: the
 * last two that start at or before the span's start, every one that starts inside it, and the
 * first that starts at or past its end. An analog channel's breaks in its source's ticks: the last
 * that starts a run at or before the span's start, the one before it, and every one after it
 * through the first at or past its end.
 */
export interface Listed {
  readonly gaps?: readonly Gap[];
  /** A digital channel's first known tick, where the answer reaches a gap from its start. */
  readonly start?: number;
  readonly breaks?: readonly Break[];
  /** An analog channel's end, where the answer reaches its last run. */
  readonly end?: number;
}

/** `listed` with each of its axis ticks through `f`, as a source that moves or rescales a channel
 * moves its answers; breaks are in the source's ticks, which a channel's `map` places. */
export function listedOnAxis(listed: Listed | undefined, f: (tick: number) => number): Listed | undefined {
  if (!listed) return undefined;
  return {
    ...listed,
    ...(listed.gaps && { gaps: listed.gaps.map((g) => ({ ...g, from: f(g.from), to: f(g.to) })) }),
    ...(listed.start !== undefined && { start: f(listed.start) }),
    ...(listed.end !== undefined && { end: f(listed.end) }),
  };
}

/** The channel as an answer's span sees it: one whose gaps or breaks are in pages takes the
 * answer's, which hold for ticks in that span. Any other is itself. */
export function spanned(c: Channel, listed: Listed | undefined): Channel {
  if (c.kind === "unavailable" || !c.paged || !listed) return c;
  if (c.kind === "digital") return { ...c, gaps: listed.gaps ?? [], ...(listed.start !== undefined && { start: listed.start }) };
  return { ...c, breaks: listed.breaks ?? [], ...(listed.end !== undefined ? { end: listed.end } : { lostAfter: undefined }) };
}

/** `a / b` in lowest terms, exactly: each term below 2^53 once reduced. */
function ratioOf(a: bigint, b: bigint): Ratio {
  let [x, y] = [a, b];
  while (y !== 0n) [x, y] = [y, x % y];
  return { num: Number(a / x), den: Number(b / x) };
}

/** A source's `m` on a recording at `timebase`: how many of its ticks one of the source's lasts,
 * `stretch × R / rate` (FORMAT.md, "Times"). */
export function mOf(timebase: Ratio, source: Pick<SourceHeader, "timebase" | "stretch">): Ratio {
  const [sn, sd] = source.stretch ?? [1, 1];
  return ratioOf(
    BigInt(sn) * BigInt(timebase.num) * BigInt(source.timebase[1]),
    BigInt(sd) * BigInt(timebase.den) * BigInt(source.timebase[0]),
  );
}

/** Source `s`'s `m` in `info`. */
export function sourceM(info: RecordingInfo, s: number): Ratio {
  return mOf(info.timebase, info.sources![s]);
}

/** Whether `a` is larger than `b`. */
function larger(a: Ratio, b: Ratio): boolean {
  return BigInt(a.num) * BigInt(b.den) > BigInt(b.num) * BigInt(a.den);
}

/**
 * The recording's ticks one tick of a reading's source spans, its `m` (FORMAT.md, "Times"): a
 * reading of channels keeps to the coarsest of their sources' ticks, and one tied to no channel to
 * the finest source's. 1 for a recording of one source with no stretch, or one that lists none.
 */
export function readingStep(info: RecordingInfo, channels: readonly number[] = []): Ratio {
  const sources = info.sources;
  if (!sources || sources.length === 0) return { num: 1, den: 1 };
  const of = channels.map((c) => info.channels[c].source).filter((s): s is number => s !== undefined);
  const ms = (of.length > 0 ? of : sources.map((_, s) => s)).map((s) => sourceM(info, s));
  const pick = of.length > 0 ? (a: Ratio, b: Ratio) => (larger(b, a) ? b : a) : (a: Ratio, b: Ratio) => (larger(a, b) ? b : a);
  return ms.reduce(pick);
}

/** A recording's content id: "blake3:" and the 64 hex digits of its content's digest. */
export type ContentId = `blake3:${string}`;

/** The kind of capture a recording was converted from. A newer converter may write another name. */
export type SourceFormat = "saleae-binary-0" | "saleae-binary-1" | "sigrok-sr" | "vcd" | (string & {});

/** How the converter came by the sample rate, where the source doesn't record it. */
export type RateOrigin = "inferred" | "given";

/** Where a recording came from, as its header states it (FORMAT.md, "Header"). */
export interface SourceHeader {
  readonly format: SourceFormat;
  /** File names, without directories; none for a recorder's own stream. */
  readonly files: readonly string[];
  /** The source's ticks a second, `[num, den]`. */
  readonly timebase: readonly [number, number];
  /** The recording's tick where the source's tick 0 lies, before a channel's offset. */
  readonly origin: number;
  /** The recording's ticks it was moved by against the other sources to line instruments up: a
   * claim. */
  readonly align?: number;
  /** `[num, den]`: how much a merge lengthened the source's ticks against its timebase, to line
   * up a clock that ran at another rate than it states: a claim. Absent: 1. */
  readonly stretch?: readonly [number, number];
  readonly instrument?: string;
  readonly software?: string;
  /** The source's own time at its tick 0, in seconds. */
  readonly begin_s?: number;
  /** The trigger's time, in the same seconds. */
  readonly trigger_s?: number;
  readonly rate?: RateOrigin;
  /** Anything else the source states. */
  readonly extra?: Readonly<Record<string, string>>;
  /** The converter's name, version and arguments. */
  readonly converter?: string;
  /** The title of the recording a merge took the source from. */
  readonly title?: string;
  /** The metadata of the recording a merge took the source from. */
  readonly meta?: Readonly<Record<string, string>>;
}

export interface RecordingInfo {
  readonly title: string;
  /** Ticks a second. */
  readonly timebase: Ratio;
  /** The capture covers ticks [0, length). */
  readonly length: number;
  readonly channels: readonly Channel[];
  /** The recording's download size in bytes, where the source knows it. */
  readonly bytes?: number;
  /** What a .tickwave header states: its content id, where its channels came from, and free
   * metadata. */
  readonly contentId?: ContentId;
  readonly sources?: readonly SourceHeader[];
  readonly meta?: Readonly<Record<string, string>>;
  /** The file bundles a sidecar, whose annotations a view reads when it first asks for them. */
  readonly bundled?: boolean;
}

export interface Edges {
  /** The level at `start`. */
  readonly level: 0 | 1;
  /** Every edge in [start, end), in order. Two can share a tick: a pulse the source stamped with one time. */
  readonly ticks: Float64Array;
  /** For a channel whose gaps are in pages, the gaps the span needs. */
  readonly listed?: Listed;
}

/** Buckets of `bucket` ticks, the first starting at `start`, a multiple of `bucket`, as a level
 * stores them (FORMAT.md, "Overview"). */
export interface DigitalOverview {
  readonly kind: "digital";
  readonly bucket: number;
  readonly start: number;
  /** Each bucket's density: 0 for no edge, and one step up for each quarter of a doubling of its
   * edges. */
  readonly density: Uint8Array;
  /** Each bucket's duty: its share of the ticks in the channel's kept span that are high, in
   * fourteenths rounded, 0 wholly low to 14 wholly high; 15 for a bucket wholly outside that span. */
  readonly duty: Uint8Array;
  /** For a channel whose gaps are in pages, the gaps the span needs; absent from the coarsest
   * level asked for with no limit, which a first draw reads from the footer alone. */
  readonly listed?: Listed;
}

export interface AnalogOverview {
  readonly kind: "analog";
  readonly bucket: number;
  readonly start: number;
  /** The smallest and largest value in each bucket; +Infinity and -Infinity in one with no sample. */
  readonly min: Float64Array | Float32Array;
  readonly max: Float64Array | Float32Array;
  /** For a channel whose breaks are in pages, as `DigitalOverview.listed`. */
  readonly listed?: Listed;
}

export type Overview = DigitalOverview | AnalogOverview;

export interface Samples {
  /** The index of `values[0]`. */
  readonly first: number;
  readonly values: Float64Array | Float32Array;
  /**
   * Optional: each whole run of 64 samples' smallest and largest value, NaN left out. Run r covers
   * `values[64 r]` to `values[64 r + 63]`. A source that leaves it out costs the renderer the
   * time to build what each draw reads.
   */
  readonly runs?: SampleRuns;
  /** For a channel whose breaks are in pages, the breaks the span needs. */
  readonly listed?: Listed;
}

export interface SampleRuns {
  readonly min: Float64Array | Float32Array;
  readonly max: Float64Array | Float32Array;
}

/** Samples a run of `Samples.runs` covers. */
export const SAMPLES_PER_RUN = 64;

/** Sets runs [r0, r1) of `min` and `max` from `values`. */
export function fillRuns(values: Samples["values"], min: SampleRuns["min"], max: SampleRuns["max"], r0: number, r1: number): void {
  for (let r = r0; r < r1; r++) {
    let lo = Infinity;
    let hi = -Infinity;
    for (let i = r * SAMPLES_PER_RUN; i < (r + 1) * SAMPLES_PER_RUN; i++) {
      if (values[i] < lo) lo = values[i];
      if (values[i] > hi) hi = values[i];
    }
    min[r] = lo;
    max[r] = hi;
  }
}

export interface Source {
  info(signal?: AbortSignal): Promise<RecordingInfo>;

  edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges>;

  /** How many edges a digital channel has in [start, end), counted without reading them all where
   * the source can: a `.tickwave` file's from its index and the blocks at the span's ends. A
   * source without it is counted from `edges`. */
  edgeCount?(channel: number, start: number, end: number, signal?: AbortSignal): Promise<number>;

  /** A digital channel's periods in [start, end), rising edge to rising edge, as `periodStats`
   * gives them, worked out where the edges are read, so none of them comes back to the caller.
   * A source without it is measured from `edges`. */
  periods?(channel: number, start: number, end: number, signal?: AbortSignal): Promise<PeriodStats | null>;

  /**
   * The coarsest overview level whose bucket is no larger than `maxBucket` ticks, covering
   * [start, end). Null when even the finest level is coarser than that, or the source has none:
   * the caller then reads edges or samples. With no limit, `Infinity`, it is the first draw's:
   * the coarsest level, from what the footer holds alone, so it carries no `listed`.
   */
  overview(
    channel: number,
    start: number,
    end: number,
    maxBucket: number,
    signal?: AbortSignal,
  ): Promise<Overview | null>;

  /** An analog channel's samples whose ticks fall in [start, end). */
  samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples>;

  /** Fetch what `info` and the coarse overview need, ahead of a view. Never rejects. */
  prefetch(): Promise<void>;
}

/** The tick of sample i: first + floor(i * period), as FORMAT.md places it. */
export function sampleTick(first: number, period: Ratio, i: number): number {
  const product = i * period.num;
  if (Number.isSafeInteger(product)) return first + Math.floor(product / period.den);
  return first + Number((BigInt(i) * BigInt(period.num)) / BigInt(period.den));
}

/** The index of the first sample whose tick is at or after `tick`, clamped to `0..=count`. */
export function sampleIndexAtOrAfter(first: number, period: Ratio, count: number, tick: number): number {
  let i = Math.ceil(((tick - first) * period.den) / period.num);
  // The estimate can land one off either way; settle it against the sample ticks themselves.
  i = Math.min(Math.max(i, 0), count);
  while (i > 0 && sampleTick(first, period, i - 1) >= tick) i--;
  while (i < count && sampleTick(first, period, i) < tick) i++;
  return i;
}

/**
 * The index of the last sample at or before axis tick `tick`: the value a cursor there reads. Null
 * before the first sample or past the channel's end.
 */
export function sampleAtOrBefore(c: AnalogChannel, tick: number): number | null {
  if (tick < channelStart(c) || tick >= c.end || c.count === 0) return null;
  const i = firstPast(c, tick, true) - 1;
  return i < 0 ? null : Math.min(i, c.count - 1);
}

/**
 * An error whose `name` says what went wrong, so a page can explain it: `TickwaveNetworkError` (no
 * answer, which a refused cross-origin request looks like too), `TickwaveHttpError` (an answer that
 * was not the bytes), `TickwaveNotRecording`, `TickwaveSidecarError`, `TickwaveNoLoader` (a URL no
 * loader reads).
 */
export function named(name: string, message: string): Error {
  const e = new Error(message);
  e.name = name;
  return e;
}

/** The DOMException a source rejects with when its signal aborts. */
export function aborted(): DOMException {
  return new DOMException("aborted", "AbortError");
}

export function isAbort(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}
