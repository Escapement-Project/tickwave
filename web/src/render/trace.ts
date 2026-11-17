// A digital row's trace from its columns and their looks (shade.ts), in device pixels. Pure, so
// the shapes can be tested without a canvas: the line's pen is a Path2D when the plot draws.

import { FALL, HIGH, LOW, NONE, RISE, type DigitalColumns } from "./columns.ts";
import { HELD, PULSE, SHADE, STROKE, type Looks } from "./shade.ts";

/** What a trace draws its line into, stroked at the trace's width. A Path2D is one. */
export interface Pen {
  moveTo(x: number, y: number): void;
  lineTo(x: number, y: number): void;
}

/** Where a trace sits: column 0 at `left`, the high level at `yH`, the low at `yL`, its line `lw` wide. */
export interface TraceBand {
  readonly left: number;
  readonly yH: number;
  readonly yL: number;
  readonly lw: number;
}

/** The opacities a trace fills at, in steps of 1 / SHADES: finer than a canvas's 8-bit alpha
 * shows apart at these sizes. */
export const SHADES = 64;

/** `alpha` in steps of 1 / SHADES; at least one step for any opacity above none. */
export function shadeStep(alpha: number): number {
  if (!(alpha > 0)) return 0;
  return Math.max(1, Math.min(SHADES, Math.round(alpha * SHADES)));
}

/**
 * What a trace fills, as each column's alpha from 0 to 255, column 0 at the band's `left`: the band
 * between the levels, and each level's rail, a line's width deep about it. The plot draws each as
 * a strip of one-pixel columns, so a rail whose opacity changes at every column costs what a flat
 * one does.
 */
export interface Fills {
  readonly band: Uint8ClampedArray;
  readonly high: Uint8ClampedArray;
  readonly low: Uint8ClampedArray;
}

/**
 * `count` columns' trace, each drawn as its look says. Flat columns and edges at full ink are a
 * line, stroked; a level held over several columns is one segment. An edge the model fades, and a
 * pulse, are a rectangle across the band and both rails a line's width wide at the opacity its
 * stroke takes: an edge where it sits, its levels on the line, and a pulse at its centre, the line
 * in at the level before it and out at the level after it. Each column the rectangle covers in part
 * takes that share of the opacity, as a canvas covers it. A run's columns are its band and rails at
 * theirs, and the line stops at a run's sides. The fills are given back, null where there are none.
 * Each opacity is in steps of 1 / SHADES.
 */
export function digitalTrace(cols: DigitalColumns, looks: Looks, count: number, band: TraceBand, line: Pen): Fills | null {
  const { left, yH, yL, lw } = band;
  const h = lw / 2;
  const { state, at } = cols;
  let fills: Fills | null = null;
  const alpha = (q: number) => Math.round((255 * q) / SHADES);
  /** A stroke across the band at `x`, at step `q`. Strokes never share a column: those within a
   * device pixel of each other join a run. */
  const stroke = (x: number, q: number) => {
    if (q === 0) return;
    fills ??= { band: new Uint8ClampedArray(count), high: new Uint8ClampedArray(count), low: new Uint8ClampedArray(count) };
    const [x0, x1] = [x - h - left, x + h - left];
    for (let c = Math.max(0, Math.floor(x0)); c < Math.min(count, Math.ceil(x1)); c++) {
      const a = Math.round((255 * q * (Math.min(c + 1, x1) - Math.max(c, x0))) / SHADES);
      fills.band[c] = fills.high[c] = fills.low[c] = Math.max(fills.band[c], a);
    }
  };
  // The level the line is at, null where it is lifted, and where a horizontal along it ends that
  // the line has not been given yet: the next one that carries on from there joins it.
  let pen: number | null = null;
  let to: number | null = null;
  const flush = () => {
    if (to !== null) line.lineTo(to, pen!);
    to = null;
  };
  const lift = () => {
    flush();
    pen = null;
  };
  const along = (x0: number, x1: number, y: number) => {
    if (pen !== y) {
      flush();
      line.moveTo(x0, y);
    }
    [pen, to] = [y, x1];
  };
  for (let c = 0; c < count; c++) {
    const x = left + c;
    const kind = looks.kind[c];
    const st = state[c];
    if (kind === SHADE) {
      fills ??= { band: new Uint8ClampedArray(count), high: new Uint8ClampedArray(count), low: new Uint8ClampedArray(count) };
      fills.band[c] = alpha(shadeStep(looks.band[c]));
      fills.high[c] = alpha(shadeStep(looks.high[c]));
      fills.low[c] = alpha(shadeStep(looks.low[c]));
      lift();
      continue;
    }
    if (st === NONE) {
      lift();
      continue;
    }
    if (kind === HELD) {
      fills ??= { band: new Uint8ClampedArray(count), high: new Uint8ClampedArray(count), low: new Uint8ClampedArray(count) };
      (st === HIGH ? fills.high : fills.low)[c] = alpha(shadeStep(looks.band[c]));
      lift();
      continue;
    }
    if (st === LOW || st === HIGH) {
      along(x, x + 1, st === HIGH ? yH : yL);
      continue;
    }
    if (kind === PULSE) {
      // As a lone edge: in at the level before its first column to its centre, a stroke across the
      // band, and out at the level after its last column.
      const mid = left + looks.at[c];
      let last = c;
      while (last + 1 < count && looks.kind[last + 1] === PULSE && looks.at[last + 1] === looks.at[c]) last++;
      const before: number | null = st === RISE ? yL : st === FALL ? yH : pen;
      if (before !== null) along(x, mid, before);
      stroke(mid, shadeStep(looks.band[c]));
      const end = state[last];
      const next = state[last + 1];
      const after =
        end === RISE ? yH : end === FALL ? yL : next === HIGH || next === FALL ? yH : next === LOW || next === RISE ? yL : before;
      lift();
      if (after !== null) along(mid, left + last + 1, after);
      c = last;
      continue;
    }
    const q = kind === STROKE ? shadeStep(looks.band[c]) : SHADES;
    const [ya, yb] = st === RISE ? [yL, yH] : [yH, yL];
    const ex = x + at[c];
    along(x, ex, ya);
    flush();
    if (q >= SHADES) line.lineTo(ex, yb);
    else {
      // Its levels at full ink, the edge itself at the opacity the model gives it.
      stroke(ex, q);
      line.moveTo(ex, yb);
    }
    [pen, to] = [yb, x + 1];
  }
  flush();
  return fills;
}
