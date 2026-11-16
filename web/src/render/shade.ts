// How a digital column that holds edges reads, the same at every zoom and in every recording.
// Its band, between the two levels' rails, takes an opacity from its edge rate, edges a second, on
// one fixed log scale; each rail takes the share of the column's time at its level. A column with
// an edge spent time at both levels, so both rails show at least the floor there, and so does the
// band: a rare edge never reads as empty. A port of crates/view/src/shade.rs, held to the same
// cases in testdata/view/shade.json.

import type { Ratio } from "../source.ts";
import { BUSY, FALL, NONE, RISE, type DigitalColumns } from "./columns.ts";

/** The opacity of a band at `RATE_FLOOR` edges a second and below, and the least a rail of a column with an edge takes. */
export const FLOOR = 0.2;
/** Edges a second at the floor. */
export const RATE_FLOOR = 1;
/** Edges a second at full opacity: a clock of 50 MHz. */
export const RATE_FULL = 1e8;

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
 * A band's opacity at `rate` edges a second: none at no edge, `FLOOR` up to `RATE_FLOOR`, full from
 * `RATE_FULL`, and linear in the rate's logarithm between.
 */
export function band(rate: number): number {
  if (!(rate > 0)) return 0;
  const along = Math.log2(rate / RATE_FLOOR) / Math.log2(RATE_FULL / RATE_FLOOR);
  return FLOOR + (1 - FLOOR) * Math.min(1, Math.max(0, along));
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
 * The high rail's opacity and the low's, for a column whose known time is `high` high, as a share
 * from 0 to 1, NaN where none of it is known. With `edges`, each is at least `FLOOR`.
 */
export function rails(high: number, edges: boolean): [number, number] {
  if (Number.isNaN(high)) return [0, 0];
  const h = Math.min(1, Math.max(0, high));
  const l = Math.min(1, Math.max(0, 1 - high));
  return edges ? [Math.max(h, FLOOR), Math.max(l, FLOOR)] : [h, l];
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
 * How a column of a digital row draws: its level as the line; a stroke across the band a line's
 * width wide at an opacity (a lone edge where it sits, a busy column alone at its middle); or, in
 * a run, the band between the levels and each level's rail at theirs.
 */
export type Look = "flat" | ["stroke", number] | ["shade", number, number, number];

/** The columns either side of one in a run whose time its rails take: a centred window of five device pixels, the odd width nearest four, held to the run. */
export const RAIL_REACH = 2;

/**
 * Each of columns c0..c1's look, on a grid of a device pixel a column `per` ticks wide, on an axis
 * of `timebase` ticks a second, the line `width` device pixels wide. Edges whose strokes come
 * within a device pixel of each other, or of a busy column, join in a run with the columns between
 * them; a column with no data ends a run. A run's band takes the rate of its edges over its
 * columns, and each of its columns' rails the share of time high over the `RAIL_REACH` columns
 * either side of it within the run, at least the floor where those hold an edge. A lone edge, or a
 * busy column with no other changing column within a device pixel, is a stroke at the opacity
 * `stroke` gives the spacing to its nearest neighbour, at the rate of its own edges, at least one,
 * over that spacing. Columns outside c0..c1 are flat.
 */
export function looks(cols: DigitalColumns, c0: number, c1: number, width: number, per: number, timebase: Ratio): Look[] {
  const { state, at } = cols;
  const out: Look[] = Array.from({ length: state.length }, () => "flat");
  // Each changing column, its centre in columns, and how far its paint reaches either side of it.
  const changing: [number, number, number][] = [];
  for (let k = c0; k < c1; k++) {
    if (state[k] === RISE || state[k] === FALL) changing.push([k, k + at[k], width / 2]);
    else if (state[k] === BUSY) changing.push([k, k + 0.5, 0.5 + (width - 1) / 2]);
  }
  // From centre to centre, and the bare space between their paint.
  const gap = (i: number, j: number): [number, number] => {
    const [[, a, ra], [, b, rb]] = [changing[i], changing[j]];
    return [b - a, b - rb - (a + ra)];
  };
  const noData = (a: number, b: number) => {
    for (let k = a; k < b; k++) if (state[k] === NONE) return true;
    return false;
  };
  const sum = (of: Float64Array, a: number, b: number) => {
    let s = 0;
    for (let k = a; k <= b; k++) s += of[k];
    return s;
  };
  for (let i = 0; i < changing.length; ) {
    let last = i;
    while (last + 1 < changing.length && gap(last, last + 1)[1] < 1 && !noData(changing[last][0], changing[last + 1][0])) last++;
    const [k0, k1] = [changing[i][0], changing[last][0]];
    if (last === i) {
      let spacing = Infinity;
      if (i > 0) spacing = Math.min(spacing, gap(i - 1, i)[0]);
      if (i + 1 < changing.length) spacing = Math.min(spacing, gap(i, i + 1)[0]);
      // A busy column changes level at least once, though a level's may hold no edge (a gap's end).
      const edges = Math.max(cols.edges[k0], 1);
      out[k0] = ["stroke", stroke(band(rate(edges, spacing * per, timebase)), spacing, width)];
    } else {
      const b = band(rate(sum(cols.edges, k0, k1), (k1 - k0 + 1) * per, timebase));
      for (let k = k0; k <= k1; k++) {
        const [a, z] = [Math.max(k0, k - RAIL_REACH), Math.min(k1, k + RAIL_REACH)];
        const known = sum(cols.known, a, z);
        const [high, low] = rails(known > 0 ? sum(cols.high, a, z) / known : NaN, sum(cols.edges, a, z) > 0);
        out[k] = ["shade", b, high, low];
      }
    }
    i = last + 1;
  }
  return out;
}
