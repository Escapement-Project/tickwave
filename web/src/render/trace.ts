// A digital row's trace from its columns and their looks (shade.ts), in device pixels. Pure, so
// the shapes can be tested without a canvas: the pens are Path2Ds when the plot draws.

import { BUSY, FALL, HIGH, LOW, NONE, RISE, type DigitalColumns } from "./columns.ts";
import type { Look } from "./shade.ts";

/** What a trace draws into: its line, stroked at the trace's width, and its fills. A Path2D is one. */
export interface Pen {
  moveTo(x: number, y: number): void;
  lineTo(x: number, y: number): void;
  rect(x: number, y: number, w: number, h: number): void;
}

/** Where a trace sits: column 0 at `left`, the high level at `yH`, the low at `yL`, its line `lw` wide. */
export interface TraceBand {
  readonly left: number;
  readonly yH: number;
  readonly yL: number;
  readonly lw: number;
}

/** The opacities a trace fills at, in steps of 1 / SHADES: fewer fills a frame, and finer than a
 * canvas's 8-bit alpha shows apart at these sizes. */
export const SHADES = 64;

/** `alpha` in steps of 1 / SHADES; at least one step for any opacity above none. */
export function shadeStep(alpha: number): number {
  if (!(alpha > 0)) return 0;
  return Math.max(1, Math.min(SHADES, Math.round(alpha * SHADES)));
}

/**
 * `count` columns' trace, each drawn as its look says. Flat columns and edges at full ink are a
 * line, stroked. An edge the model fades, and a busy column alone, are a rectangle across the band
 * a line's width wide at the opacity its stroke takes: an edge where it sits, its levels on the
 * line, and a busy column at its middle. A run's column is its band between the levels and each
 * level's rail, a line's width deep, at theirs; the line stops at the run's sides. `shade(q)` is
 * the pen that fills at opacity `q / SHADES`; neighbouring columns of one opacity are one
 * rectangle.
 */
export function digitalTrace(cols: DigitalColumns, looks: readonly Look[], count: number, band: TraceBand, line: Pen, shade: (q: number) => Pen): void {
  const { left, yH, yL, lw } = band;
  const h = lw / 2;
  const { state, at } = cols;
  // A run's three layers, the band and each rail: each an open rectangle from x0 to x1 at step q,
  // until a column's differs.
  const layers = [
    { y: yH + h, height: yL - yH - lw, q: 0, x0: 0, x1: 0 },
    { y: yH - h, height: lw, q: 0, x0: 0, x1: 0 },
    { y: yL - h, height: lw, q: 0, x0: 0, x1: 0 },
  ];
  type Layer = (typeof layers)[number];
  const close = (l: Layer) => {
    if (l.q > 0 && l.x1 > l.x0 && l.height > 0) shade(l.q).rect(l.x0, l.y, l.x1 - l.x0, l.height);
    l.q = 0;
  };
  const paint = (l: Layer, x: number, q: number) => {
    if (l.q === q && l.x1 === x) {
      l.x1 = x + 1;
      return;
    }
    close(l);
    [l.q, l.x0, l.x1] = [q, x, x + 1];
  };
  /** A stroke across the band at `x`, at step `q`. */
  const stroke = (x: number, q: number) => {
    if (q > 0) shade(q).rect(x - h, yH - h, lw, yL - yH + lw);
  };
  let pen: number | null = null;
  const along = (x0: number, x1: number, y: number) => {
    if (pen !== y) line.moveTo(x0, y);
    line.lineTo(x1, y);
    pen = y;
  };
  for (let c = 0; c < count; c++) {
    const x = left + c;
    const look = looks[c];
    const st = state[c];
    if (look !== "flat" && look[0] === "shade") {
      paint(layers[0], x, shadeStep(look[1]));
      paint(layers[1], x, shadeStep(look[2]));
      paint(layers[2], x, shadeStep(look[3]));
      pen = null;
      continue;
    }
    if (st === NONE) {
      pen = null;
      continue;
    }
    if (st === LOW || st === HIGH) {
      along(x, x + 1, st === HIGH ? yH : yL);
      continue;
    }
    const q = look === "flat" ? SHADES : shadeStep(look[1]);
    if (st === BUSY) {
      // Alone, as a lone edge: in at the level before it to its middle, a stroke across the
      // band, and out at the level the next column starts at.
      const mid = x + 0.5;
      const before: number | null = pen;
      if (before !== null) along(x, mid, before);
      stroke(mid, q);
      const next = state[c + 1];
      const after = next === HIGH || next === FALL ? yH : next === LOW || next === RISE ? yL : before;
      pen = null;
      if (after !== null) along(mid, x + 1, after);
      continue;
    }
    const [ya, yb] = st === RISE ? [yL, yH] : [yH, yL];
    const ex = x + at[c];
    along(x, ex, ya);
    if (q >= SHADES) line.lineTo(ex, yb);
    else {
      // Its levels at full ink, the edge itself at the opacity the model gives it.
      stroke(ex, q);
      line.moveTo(ex, yb);
    }
    line.lineTo(x + 1, yb);
    pen = yb;
  }
  for (const l of layers) close(l);
}
