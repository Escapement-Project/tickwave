// How a digital column that holds edges reads, the same at every zoom and in every recording.
// Edges that stand apart are strokes, and so is a pulse or a burst too short to have a stretch to
// shade. Where edges crowd, a run's band, between the two levels' rails, takes an opacity from its
// edge rate, edges a second, on one fixed log scale; each rail takes a part of the band's opacity
// by its level's share of the time. A column with an edge spent time at both levels, so both rails
// show at least RAIL_FLOOR of the band there, and the band itself at least FLOOR: a rare edge never
// reads as empty. A port of crates/view/src/shade.rs, held to the same cases in
// testdata/view/shade.json.

import type { Ratio } from "../source.ts";
import { BUSY, FALL, NONE, RISE, type DigitalColumns } from "./columns.ts";

/** The opacity of a band at `RATE_FLOOR` edges a second and below. */
export const FLOOR = 0.2;
/** The opacity of a band at `RATE_TOP` edges a second and above: short of full ink, so the busiest band still reads as a fill. */
export const TOP = 0.6;
/** Edges a second at the floor. */
export const RATE_FLOOR = 1;
/** Edges a second at the top: a clock of 50 MHz. */
export const RATE_TOP = 1e8;
/** The least part of the band's opacity a rail takes where the columns about it hold an edge. */
export const RAIL_FLOOR = 0.2;

/** `ceil(2^(63 + r/4))` for r = 1, 2, 3: FORMAT.md's thresholds between a doubling's steps. */
const STEPS = [0x9837f0518db8a970n, 0xb504f333f9de6485n, 0xd744fccad69d6af5n];

/** The smallest edge count whose density is at least `d`, from 1; null past what a u64 counts. */
function least(d: number): bigint | null {
  const l = Math.floor((d - 1) / 4);
  const r = (d - 1) % 4;
  if (l > 63) return null;
  if (r === 0) return 1n << BigInt(l);
  // The smallest e in [2^l, 2^(l + 1)) with `e << (63 − l)` at least the step.
  return ((STEPS[r - 1] - 1n) >> BigInt(63 - l)) + 1n;
}

/**
 * The smallest and largest edge count density `d` stands for, null for 0. A step no count reaches,
 * as 2, 3 and 4 between one edge and two, stands for the next count up alone.
 */
export function counts(d: number): [bigint, bigint] | null {
  if (d === 0) return null;
  const lo = least(d);
  if (lo === null) return null;
  const next = least(d + 1);
  const hi = next === null ? (1n << 64n) - 1n : next - 1n;
  return [lo, hi > lo ? hi : lo];
}

/** The density a bucket of `edges` edges stores (FORMAT.md, "Overview"): 0 for none, and
 * otherwise `1 + floor(4 × log2(edges))`, worked in integers as the format words it. */
export function densityOf(edges: number): number {
  if (edges <= 0) return 0;
  const e = BigInt(edges);
  const l = e.toString(2).length - 1;
  const x = e << BigInt(63 - l);
  return 1 + 4 * l + STEPS.filter((t) => x >= t).length;
}

/** The duty a bucket stores whose `known` ticks in the channel's kept span hold `high` high ones
 * (FORMAT.md, "Overview"): 15 where none is, else `14 × high / known` rounded, a half up. */
export function dutyOf(high: number, known: number): number {
  if (known <= 0) return 15;
  const [h, k] = [BigInt(high), BigInt(known)];
  return Number((28n * h + k) / (2n * k));
}

/**
 * The edge count density `d` stands for: the geometric mean of the least and most it stands for,
 * so a count it stands for lies within `sqrt(most / least) − 1` of it, under 9.1 % at any density,
 * and exactly where it stands for one count, as for one to seven edges.
 */
export function count(d: number): number {
  const c = counts(d);
  return c === null ? 0 : Math.sqrt(Number(c[0]) * Number(c[1]));
}

/** Each density's count, worked once. */
const COUNTS = Float64Array.from({ length: 256 }, (_, d) => count(d));

/** `count`, from a table. */
export const countOf = (d: number): number => COUNTS[d] ?? 0;

/** Edges a second: `edges` over `ticks` of the axis, which runs `timebase` ticks a second. */
export function rate(edges: number, ticks: number, timebase: Ratio): number {
  return (edges * timebase.num) / (ticks * timebase.den);
}

/**
 * A band's opacity at `rate` edges a second: none at no edge, `FLOOR` up to `RATE_FLOOR`, `TOP` from
 * `RATE_TOP`, and linear in the rate's logarithm between.
 */
export function band(rate: number): number {
  if (!(rate > 0)) return 0;
  const along = Math.log2(rate / RATE_FLOOR) / Math.log2(RATE_TOP / RATE_FLOOR);
  return FLOOR + (TOP - FLOOR) * Math.min(1, Math.max(0, along));
}

/**
 * The opacity of a lone edge's stroke, `width` device pixels wide and `spacing` from the nearest
 * edge, centre to centre, where `band` is the opacity of a band at the rate that spacing stands
 * for, an edge every `spacing`: the stroke covers its stretch of band as darkly as that band
 * would, up to full ink. An edge with none beside it takes full ink.
 */
export function stroke(band: number, spacing: number, width: number): number {
  if (!Number.isFinite(spacing)) return 1;
  return Math.min(1, (band * spacing) / width);
}

/**
 * The high rail's opacity and the low's, for columns whose known time is `high` high, as a share
 * from 0 to 1, NaN where none of it is known, in a run whose band takes opacity `band`. With
 * `edges`, each rail takes the band's opacity times its level's share over the larger share, at
 * least `RAIL_FLOOR` of it: an even wave's rails both at the band's, 75 % high the high rail at it
 * and the low at a third of it. Without, each takes the band's opacity times its share, so a level
 * held inside a run reads in the band's tone, not as a line at full ink.
 */
export function rails(high: number, edges: boolean, band: number): [number, number] {
  if (Number.isNaN(high)) return [0, 0];
  const h = Math.min(1, Math.max(0, high));
  const l = Math.min(1, Math.max(0, 1 - high));
  if (!edges) return [band * h, band * l];
  const larger = Math.max(h, l);
  return [band * Math.max(h / larger, RAIL_FLOOR), band * Math.max(l / larger, RAIL_FLOOR)];
}

/**
 * The share of its time a bucket of duty `duty` spends high, as `[stands for, least, most]`: duty
 * `d` stands for `d / 14`, rounded from any share at least `(2d − 1) / 28` and below
 * `(2d + 1) / 28`, held to 0..1. null for 15, a bucket outside the channel's kept span.
 */
export function dutyShare(duty: number): [number, number, number] | null {
  if (duty > 14) return null;
  return [duty / 14, Math.max(0, (2 * duty - 1) / 28), Math.min(1, (2 * duty + 1) / 28)];
}

/**
 * How a column of a digital row draws: its level as the line; a lone edge's stroke across the band
 * a line's width wide at an opacity, where it sits; a busy column alone or a short run as one such
 * stroke at an opacity, centred at a place in columns from the row's first; in a run, the band
 * between the levels and each level's rail at theirs; or, held briefly between two runs, its level's
 * line at an opacity.
 */
export type Look = "flat" | ["stroke", number] | ["pulse", number, number] | ["shade", number, number, number] | ["held", number];

/** `Looks.kind`: the column's level as the line. */
export const FLAT = 0;
/** `Looks.kind`: a lone edge's stroke across the band at opacity `band`. */
export const STROKE = 1;
/** `Looks.kind`: in a run, the band at `band` and the rails at `high` and `low`. */
export const SHADE = 2;
/**
 * `Looks.kind`: a busy column alone or a short run, one stroke across the band at opacity `band`
 * centred at `at`, in columns from the row's first. Each of its columns holds it. The line comes in
 * at the level before its first column and leaves at the level the column after its last starts at.
 */
export const PULSE = 3;
/** `Looks.kind`: a level held briefly between two runs: no band, and its line at opacity `band`, the
 * larger of the two runs' bands. */
export const HELD = 4;

/** Each column's `Look`, a field to an array, so a row's looks are five arrays and no object a column. */
export interface Looks {
  readonly kind: Uint8Array;
  readonly band: Float64Array;
  readonly high: Float64Array;
  readonly low: Float64Array;
  readonly at: Float64Array;
}

/** Column `k`'s look. */
export function lookAt(l: Looks, k: number): Look {
  if (l.kind[k] === STROKE) return ["stroke", l.band[k]];
  if (l.kind[k] === PULSE) return ["pulse", l.band[k], l.at[k]];
  if (l.kind[k] === SHADE) return ["shade", l.band[k], l.high[k], l.low[k]];
  if (l.kind[k] === HELD) return ["held", l.band[k]];
  return "flat";
}

/** The columns either side of one in a run whose time its rails take: a centred window of five device pixels, the odd width nearest four, held to the run. */
export const RAIL_REACH = 2;

/**
 * Each of columns c0..c1's look, on a grid of a device pixel a column `per` ticks wide, on an axis
 * of `timebase` ticks a second, the line `width` device pixels wide. Edges whose strokes come
 * within a device pixel of each other, or of a busy column, join in a run with the columns between
 * them; a column with no data ends a run. So two changing columns join when their centres are under
 * `width + 1` apart, the join distance.
 *
 * A run whose first and last changing columns are under the join distance apart is short: any two
 * of its edges would join on their own, so it has no stretch for a band to describe, and a shift of
 * the column grid could have put a busy column alone in its place. It is a pulse at its middle, as
 * a busy column alone is at its own. A lone edge is a stroke. Each takes the opacity `stroke` gives
 * the spacing from its centre to its nearest neighbour's (a pulse's middle, any other changing
 * column's own centre), at the rate of its own edges, at least one, over that spacing: full ink
 * where it stands apart. A pulse's spacing counts no pulse beside it, so pulses near each other,
 * rare flags close together, each take full ink at every zoom.
 *
 * A longer run's band takes the rate of its edges over its columns, and each of its columns' rails
 * `rails` of the share of time high over the `RAIL_REACH` columns either side of it within the run.
 * Where two longer runs have under the join distance of bare space between them, and no column
 * without data, the level held between keeps its gap but is held at the larger of their bands: at
 * full ink it would draw as a speck darker than the runs either side.
 * Columns outside c0..c1 are flat.
 */
export function looks(cols: DigitalColumns, c0: number, c1: number, width: number, per: number, timebase: Ratio): Looks {
  const { state, at, edges, high, known } = cols;
  const m = state.length;
  const out: Looks = { kind: new Uint8Array(m), band: new Float64Array(m), high: new Float64Array(m), low: new Float64Array(m), at: new Float64Array(m) };
  // Each changing column, its centre in columns, and how far its paint reaches either side of it.
  const span = Math.max(0, c1 - c0);
  const [ks, centre, reach] = [new Int32Array(span), new Float64Array(span), new Float64Array(span)];
  let n = 0;
  for (let k = c0; k < c1; k++) {
    const s = state[k];
    if (s !== RISE && s !== FALL && s !== BUSY) continue;
    ks[n] = k;
    centre[n] = s === BUSY ? k + 0.5 : k + at[k];
    reach[n++] = s === BUSY ? 0.5 + (width - 1) / 2 : width / 2;
  }
  // The bare space between changing columns i's and j's paint, and whether a column with no data
  // lies between them.
  const gap = (i: number, j: number) => centre[j] - reach[j] - (centre[i] + reach[i]);
  const cut = (i: number, j: number) => {
    for (let k = ks[i]; k < ks[j]; k++) if (state[k] === NONE) return true;
    return false;
  };
  // Each run as its first and last changing column and 1 if it is short; where each changing column
  // sits as a neighbour, a short run's at its middle, as it draws, any other's at its own centre;
  // and which are in pulses.
  const place = centre.slice(0, n);
  const pulse = new Uint8Array(n);
  const runs: number[] = [];
  for (let i = 0; i < n; ) {
    let last = i;
    while (last + 1 < n && gap(last, last + 1) < 1 && !cut(last, last + 1)) last++;
    const short = centre[last] - centre[i] < width + 1;
    if (short) {
      const mid = (centre[i] + centre[last]) / 2;
      const p = last > i || state[ks[i]] === BUSY ? 1 : 0;
      for (let j = i; j <= last; j++) {
        place[j] = mid;
        pulse[j] = p;
      }
    }
    runs.push(i, last, short ? 1 : 0);
    i = last + 1;
  }
  // The longer run just before: its last changing column, -1 for none, and its band.
  let end = -1;
  let was = 0;
  for (let r = 0; r < runs.length; r += 3) {
    const [i, last] = [runs[r], runs[r + 1]];
    const [k0, k1] = [ks[i], ks[last]];
    if (runs[r + 2] === 1) {
      end = -1;
      const mid = place[i];
      let spacing = Infinity;
      if (i > 0 && !(pulse[i] && pulse[i - 1])) spacing = Math.min(spacing, mid - place[i - 1]);
      if (last + 1 < n && !(pulse[i] && pulse[last + 1])) spacing = Math.min(spacing, place[last + 1] - mid);
      // A busy column changes level at least once, though a level's may hold no edge (a gap's end).
      let count = 0;
      for (let k = k0; k <= k1; k++) count += edges[k];
      const alpha = stroke(band(rate(Math.max(count, 1), spacing * per, timebase)), spacing, width);
      if (!pulse[i]) {
        out.kind[k0] = STROKE;
        out.band[k0] = alpha;
      } else {
        for (let k = k0; k <= k1; k++) {
          out.kind[k] = PULSE;
          out.band[k] = alpha;
          out.at[k] = mid;
        }
      }
    } else {
      let all = 0;
      for (let k = k0; k <= k1; k++) all += edges[k];
      const b = band(rate(all, (k1 - k0 + 1) * per, timebase));
      for (let k = k0; k <= k1; k++) {
        const z = Math.min(k1, k + RAIL_REACH);
        let [e, h, t] = [0, 0, 0];
        for (let j = Math.max(k0, k - RAIL_REACH); j <= z; j++) {
          e += edges[j];
          h += high[j];
          t += known[j];
        }
        const [hi, lo] = rails(t > 0 ? h / t : NaN, e > 0, b);
        out.kind[k] = SHADE;
        out.band[k] = b;
        out.high[k] = hi;
        out.low[k] = lo;
      }
      if (end >= 0 && gap(end, i) < width + 1 && !cut(end, i)) {
        for (let k = ks[end] + 1; k < k0; k++) {
          out.kind[k] = HELD;
          out.band[k] = Math.max(was, b);
        }
      }
      end = last;
      was = b;
    }
  }
  return out;
}
