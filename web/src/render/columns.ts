// What each pixel column of a row shows. Column c covers ticks [x0 + c * per, x0 + (c + 1) * per).
// Pure functions, so the drawing rules can be tested without a canvas.

import { lowerBound } from "../search.ts";
import { sampleTick, type AnalogOverview, type DigitalOverview, type Edges, type Ratio, type Samples } from "../source.ts";

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
  const t = edges.ticks;
  const [c0, c1] = columnRange(g, from, to);
  const colStart = (c: number) => Math.max(from, g.x0 + c * g.per);
  let i = lowerBound(t, colStart(c0));
  let level = edges.level ^ (i & 1);
  for (let c = c0; c < c1; c++) {
    const end = Math.min(to, g.x0 + (c + 1) * g.per);
    const j = lowerBound(t, end, i);
    const n = j - i;
    if (n === 0) state[c] = level ? HIGH : LOW;
    else if (n === 1) {
      state[c] = level ? FALL : RISE;
      at[c] = (t[i] - (g.x0 + c * g.per)) / g.per;
    } else state[c] = BUSY;
    level ^= n & 1;
    i = j;
  }
  return { state, at };
}

/**
 * Columns from an overview whose buckets are no wider than a column. A column any changing bucket
 * overlaps is busy: the overview cannot say where in the bucket the change was, or how many.
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
  return { state, at };
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

/** Each column's range from the samples in it. For several samples a column. */
export function analogFromSamples(
  g: Grid,
  s: Samples,
  first: number,
  period: Ratio,
  from: number,
  to: number,
): AnalogColumns {
  const cols = emptyColumns(g.count);
  const { min, max } = cols;
  for (let k = 0; k < s.values.length; k++) {
    const t = sampleTick(first, period, s.first + k);
    if (t < from || t >= to) continue;
    const c = Math.floor((t - g.x0) / g.per);
    if (c < 0 || c >= g.count) continue;
    const v = s.values[k];
    // NaN samples are left out, as the overview leaves them out.
    if (v < min[c]) min[c] = v;
    if (v > max[c]) max[c] = v;
  }
  connect(cols);
  return cols;
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
