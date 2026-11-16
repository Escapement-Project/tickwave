// What each pixel column of a row shows. Column c covers ticks [x0 + c * per, x0 + (c + 1) * per).
// Pure functions, so the drawing rules can be tested without a canvas.

import { lowerBound } from "../search.ts";
import { countOf } from "./shade.ts";
import {
  channelIndexAtOrAfter,
  fillRuns,
  SAMPLES_PER_RUN,
  type AnalogChannel,
  type AnalogOverview,
  type DigitalOverview,
  type Edges,
  type SampleRuns,
  type Samples,
} from "../source.ts";

export const NONE = 0;
export const LOW = 1;
export const HIGH = 2;
/** More than one edge in the column, or the overview says it changes: both levels joined. */
export const BUSY = 3;
export const RISE = 4;
export const FALL = 5;

export interface DigitalColumns {
  readonly state: Uint8Array;
  /** For RISE and FALL, where in the column the edge sits, from 0 to 1. */
  readonly at: Float64Array;
  /**
   * The edges in each column: counted from edges, and from a level's buckets each bucket's
   * `shade.countOf` times the share of the bucket the column covers.
   */
  readonly edges: Float64Array;
  /**
   * The ticks of each column at the high level, of those in `known`: from edges exactly, from a
   * level's buckets each bucket's ticks in the column at its duty's share.
   */
  readonly high: Float64Array;
  /** The ticks of each column whose level is drawn: in the channel's data, and from a level's buckets, in its kept span. */
  readonly known: Float64Array;
}

/** Buckets of `bucket` ticks from `start`, as a level stores them (FORMAT.md, "Overview"). */
export interface DigitalLevels {
  readonly bucket: number;
  readonly start: number;
  /** Each bucket's density: 0 for no edge, one step up a quarter of a doubling of its edges. */
  readonly density: Uint8Array;
  /** Each bucket's duty: 0 wholly low to 14 wholly high, 15 wholly outside the kept span. */
  readonly duty: Uint8Array;
}

/** Column `k`'s share of its known time high, NaN where none is known. */
export function share(cols: DigitalColumns, k: number): number {
  return cols.known[k] > 0 ? cols.high[k] / cols.known[k] : NaN;
}

/** The ticks a column grid covers, clipped to [from, to): columns outside get NONE. */
export interface Grid {
  readonly x0: number;
  readonly per: number;
  readonly count: number;
}

function columnRange(g: Grid, from: number, to: number): [number, number] {
  const c0 = Math.max(0, Math.floor((from - g.x0) / g.per));
  const c1 = Math.min(g.count, Math.ceil((to - g.x0) / g.per));
  return [c0, Math.max(c0, c1)];
}

/**
 * Columns from exact edges fetched over [from, to). A column with no edge is flat, one with one
 * edge rises or falls where the edge is, one with more is busy.
 */
export function digitalFromEdges(g: Grid, edges: Edges, from: number, to: number): DigitalColumns {
  const state = new Uint8Array(g.count);
  const at = new Float64Array(g.count);
  const [count, high, known] = [new Float64Array(g.count), new Float64Array(g.count), new Float64Array(g.count)];
  const t = edges.ticks;
  const [c0, c1] = columnRange(g, from, to);
  const colStart = (c: number) => Math.max(from, g.x0 + c * g.per);
  let i = lowerBound(t, colStart(c0));
  let level = edges.level ^ (i & 1);
  for (let c = c0; c < c1; c++) {
    const start = colStart(c);
    const end = Math.min(to, g.x0 + (c + 1) * g.per);
    const j = lowerBound(t, end, i);
    const n = j - i;
    if (n === 0) state[c] = level ? HIGH : LOW;
    else if (n === 1) {
      state[c] = level ? FALL : RISE;
      at[c] = (t[i] - (g.x0 + c * g.per)) / g.per;
    } else state[c] = BUSY;
    // The time high: the falls' ticks less the rises', and the column's end where it ends high
    // less its start where it starts high. The edges alternate, the first falling where the column
    // starts high; whole ticks below 2^53 sum exactly.
    let alternate = 0;
    let k = i;
    for (; k + 1 < j; k += 2) alternate += t[k] - t[k + 1];
    if (k < j) alternate += t[k];
    // `0 - x`, not `-x`: a column with no edge, low, is high +0 ticks, not -0.
    let h = level ? alternate : 0 - alternate;
    if (level) h -= start;
    if (level ^ (n & 1)) h += end;
    count[c] = n;
    high[c] = h;
    known[c] = Math.max(0, end - start);
    level ^= n & 1;
    i = j;
  }
  return { state, at, edges: count, high, known };
}

/**
 * Columns from an overview's buckets, whatever their width against a column's. A column any
 * changing bucket overlaps is busy: the overview cannot say where in the bucket the change was, or
 * how many.
 */
export function digitalFromOverview(g: Grid, ov: DigitalOverview, from: number, to: number): DigitalColumns {
  const state = new Uint8Array(g.count);
  const at = new Float64Array(g.count);
  const n = ov.level.length;
  const lo = Math.max(from, ov.start);
  const hi = Math.min(to, ov.start + n * ov.bucket);
  const [c0, c1] = columnRange(g, lo, hi);
  for (let c = c0; c < c1; c++) {
    const s = Math.max(lo, g.x0 + c * g.per);
    const e = Math.min(hi, g.x0 + (c + 1) * g.per);
    const k0 = Math.max(0, Math.floor((s - ov.start) / ov.bucket));
    const k1 = Math.min(n, Math.ceil((e - ov.start) / ov.bucket));
    let busy = false;
    for (let k = k0; k < k1 && !busy; k++) busy = ov.changes[k] === 1;
    state[c] = busy ? BUSY : ov.level[k0] ? HIGH : LOW;
  }
  // A level's bits hold no count or share: until the web reads density and duty, none.
  return { state, at, edges: new Float64Array(g.count), high: new Float64Array(g.count), known: new Float64Array(g.count) };
}

/**
 * Columns from a level's buckets, whatever their width against a column's. A column any bucket
 * with an edge, or a level between low and high, overlaps is busy, and so is one whose buckets lie
 * at different levels: the level cannot say where the change was. A bucket counts toward a column's edges and time by the share of it
 * the column covers. A column whose buckets all lie outside the kept span draws nothing.
 */
export function digitalFromLevels(g: Grid, ov: DigitalLevels, from: number, to: number): DigitalColumns {
  const state = new Uint8Array(g.count);
  const at = new Float64Array(g.count);
  const [count, high, known] = [new Float64Array(g.count), new Float64Array(g.count), new Float64Array(g.count)];
  const n = ov.density.length;
  const lo = Math.max(from, ov.start);
  const hi = Math.min(to, ov.start + n * ov.bucket);
  const [c0, c1] = columnRange(g, lo, hi);
  for (let c = c0; c < c1; c++) {
    const s = Math.max(lo, g.x0 + c * g.per);
    const e = Math.min(hi, g.x0 + (c + 1) * g.per);
    const k0 = Math.max(0, Math.floor((s - ov.start) / ov.bucket));
    const k1 = Math.min(n, Math.ceil((e - ov.start) / ov.bucket));
    let edges = 0;
    let h = 0;
    let known1 = 0;
    let busy = false;
    // The first known bucket's level, where none has an edge: 1 high, 0 low, -1 none known.
    let level = -1;
    for (let k = k0; k < k1; k++) {
      const b0 = ov.start + k * ov.bucket;
      const ticks = Math.max(0, Math.min(e, b0 + ov.bucket) - Math.max(s, b0));
      // A bucket past the duties reads as outside the kept span.
      const density = ov.density[k];
      const duty = ov.duty[k] ?? 15;
      edges += (ticks / ov.bucket) * countOf(density);
      busy ||= density > 0 || (duty >= 1 && duty <= 13);
      if (duty <= 14) {
        known1 += ticks;
        h += (ticks * duty) / 14;
        // Wholly high, or wholly low: a level differing from the one before is a change with no
        // edge, a gap's end on the buckets' boundary.
        const now = duty === 14 ? 1 : 0;
        if (level < 0) level = now;
        else busy ||= level !== now;
      }
    }
    state[c] = busy ? BUSY : level < 0 ? NONE : level ? HIGH : LOW;
    count[c] = edges;
    high[c] = h;
    known[c] = known1;
  }
  return { state, at, edges: count, high, known };
}

export interface AnalogColumns {
  /**
   * The smallest and largest value a column's trace passes through. A column with none holds
   * +Infinity and -Infinity, as an empty overview bucket does.
   */
  readonly min: Float64Array;
  readonly max: Float64Array;
}

function emptyColumns(count: number): AnalogColumns {
  return { min: new Float64Array(count).fill(Infinity), max: new Float64Array(count).fill(-Infinity) };
}

/** Joins neighbouring columns: the trace passes through the gap between their ranges. */
function connect({ min, max }: AnalogColumns): void {
  for (let c = 1; c < min.length; c++) {
    if (min[c] > max[c] || min[c - 1] > max[c - 1]) continue;
    if (max[c - 1] < min[c]) min[c] = max[c - 1];
    else if (min[c - 1] > max[c]) max[c] = min[c - 1];
  }
}

const RUN = SAMPLES_PER_RUN;

// The runs of a tile whose source sent none, kept while the tile lives, so a redraw reads a
// column's whole runs rather than every sample in them. A draw builds only the runs it reads, so
// a tile's first draw pays for the view alone. A run's min is NaN until the run is built; no built
// run's is, since NaN samples are left out.
const tileRuns = new WeakMap<Samples, SampleRuns>();

/** The tile's runs, with runs [r0, r1) built. */
function runsOf(s: Samples, r0: number, r1: number): SampleRuns {
  if (s.runs) return s.runs;
  let runs = tileRuns.get(s);
  if (!runs) {
    const n = Math.floor(s.values.length / RUN);
    runs = { min: new Float64Array(n).fill(NaN), max: new Float64Array(n) };
    tileRuns.set(s, runs);
  }
  let r = r0;
  while (r < r1) {
    if (!Number.isNaN(runs.min[r])) {
      r++;
      continue;
    }
    let end = r + 1;
    while (end < r1 && Number.isNaN(runs.min[end])) end++;
    fillRuns(s.values, runs.min, runs.max, r, end);
    r = end;
  }
  return runs;
}

/** Where an analog channel's samples sit: its first, period, count and breaks, and the mapping
 * that puts them on the axis. */
export type Runs = Pick<AnalogChannel, "first" | "period" | "count" | "breaks" | "map">;

/** The tile's own index of its first sample at or after `tick`. */
function tileIndex(s: Samples, runs: Runs, tick: number): number {
  return Math.max(0, Math.min(channelIndexAtOrAfter(runs, tick), s.first + s.values.length) - s.first);
}

/** Each column's range from the samples in it. For several samples a column. */
export function analogFromSamples(
  g: Grid,
  s: Samples,
  channel: Runs,
  from: number,
  to: number,
): AnalogColumns {
  const cols = emptyColumns(g.count);
  const { min, max } = cols;
  const v = s.values;
  const [c0, c1] = columnRange(g, from, to);
  const at = (tick: number) => tileIndex(s, channel, tick);
  let i = at(Math.max(from, g.x0 + c0 * g.per));
  const runs = runsOf(s, Math.ceil(i / RUN), Math.floor(at(Math.min(to, g.x0 + c1 * g.per)) / RUN));
  for (let c = c0; c < c1; c++) {
    const j = Math.max(i, at(Math.min(to, g.x0 + (c + 1) * g.per)));
    // NaN samples are left out, as the overview leaves them out.
    let lo = Infinity;
    let hi = -Infinity;
    let k = i;
    const r0 = Math.ceil(i / RUN);
    const r1 = Math.floor(j / RUN);
    if (r0 < r1) {
      for (; k < r0 * RUN; k++) {
        if (v[k] < lo) lo = v[k];
        if (v[k] > hi) hi = v[k];
      }
      for (let r = r0; r < r1; r++) {
        if (runs.min[r] < lo) lo = runs.min[r];
        if (runs.max[r] > hi) hi = runs.max[r];
      }
      k = r1 * RUN;
    }
    for (; k < j; k++) {
      if (v[k] < lo) lo = v[k];
      if (v[k] > hi) hi = v[k];
    }
    min[c] = lo;
    max[c] = hi;
    i = j;
  }
  connect(cols);
  return cols;
}

/**
 * The samples a line through a tile's samples draws over a grid, `[k0, k1)` in the tile's own
 * indices: those in [from, to) that lie within `pad` ticks of the grid, and out to one that is not
 * NaN past each side, so the line leaves the grid on both.
 */
export function lineSamples(g: Grid, s: Samples, channel: Runs, from: number, to: number, pad: number): [number, number] {
  const v = s.values;
  const at = (tick: number) => tileIndex(s, channel, tick);
  const lo = at(from);
  const hi = at(to);
  if (hi <= lo) return [lo, lo];
  let k0 = Math.min(Math.max(lo, at(g.x0 - pad) - 1), hi - 1);
  while (k0 > lo && Number.isNaN(v[k0])) k0--;
  let k1 = Math.max(Math.min(hi, at(g.x0 + g.count * g.per + pad) + 1), lo + 1);
  while (k1 < hi && Number.isNaN(v[k1 - 1])) k1++;
  return [k0, k1];
}

/** Each column's range from the overview buckets that overlap it. */
export function analogFromOverview(g: Grid, ov: AnalogOverview, from: number, to: number): AnalogColumns {
  const cols = emptyColumns(g.count);
  const { min, max } = cols;
  const n = ov.min.length;
  const lo = Math.max(from, ov.start);
  const hi = Math.min(to, ov.start + n * ov.bucket);
  const [c0, c1] = columnRange(g, lo, hi);
  for (let c = c0; c < c1; c++) {
    const s = Math.max(lo, g.x0 + c * g.per);
    const e = Math.min(hi, g.x0 + (c + 1) * g.per);
    const k0 = Math.max(0, Math.floor((s - ov.start) / ov.bucket));
    const k1 = Math.min(n, Math.ceil((e - ov.start) / ov.bucket));
    for (let k = k0; k < k1; k++) {
      if (ov.min[k] < min[c]) min[c] = ov.min[k];
      if (ov.max[k] > max[c]) max[c] = ov.max[k];
    }
  }
  connect(cols);
  return cols;
}
