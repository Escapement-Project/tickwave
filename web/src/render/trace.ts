// A digital row's trace from its columns, in device pixels. Pure, so the shapes can be tested
// without a canvas: the pens are Path2Ds when the plot draws.

import { BUSY, FALL, HIGH, LOW, NONE, RISE, type DigitalColumns } from "./columns.ts";

/** What a trace draws into: its line, stroked at the trace's width, and its fill. A Path2D is one. */
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

const changes = (s: number) => s === BUSY || s === RISE || s === FALL;

/**
 * `count` columns' trace: flat columns and edges as a stroked line, busy columns as rectangles
 * across the band. Where `join`, edges whose strokes meet their neighbours' or a busy column's
 * are one shape with them, from the first stroke's left side to the last's right, as one stroke
 * through thousands of edges takes Firefox tens of milliseconds a frame. The shape is the area the
 * strokes paint, and a canvas strokes each segment butt-capped and fills only a join's outer
 * corner: so the band between the levels whole, but each level's margin outside it only where a
 * segment or a corner reaches, which leaves it bare where the line starts or stops inside the run's
 * sides. A column with no data ends a run. Not where the fill is drawn faded, as a coarse tile's
 * busy runs are: the flat columns' line between them would fade too.
 */
export function digitalTrace(cols: DigitalColumns, count: number, band: TraceBand, join: boolean, line: Pen, fill: Pen): void {
  const { left, yH, yL, lw } = band;
  const pad = (lw - 1) / 2;
  const top = yH - lw / 2;
  const { state, at } = cols;
  // Where a changing column paints across the band: a busy one its width and the line's overhang,
  // an edge its stroke.
  const lo = (c: number) => (state[c] === BUSY ? left + c - pad : left + c + at[c] - lw / 2);
  const hi = (c: number) => (state[c] === BUSY ? left + c + 1 + pad : left + c + at[c] + lw / 2);
  // The levels either side of an edge.
  const before = (c: number) => (state[c] === RISE ? yL : yH);
  const after = (c: number) => (state[c] === RISE ? yH : yL);
  const isEdge = (k: number) => state[k] === RISE || state[k] === FALL;
  const flatAt = (k: number, y: number) => state[k] === (y === yH ? HIGH : LOW);
  const h = lw / 2;
  // A run's bare margin places, three numbers each: level, from, to. Within a run each shape's
  // strokes reach the next one's in both margins, but for these, which a canvas leaves bare.
  const bare: number[] = [];
  const gap = (y: number, a: number, b: number) => {
    if (b > a) bare.push(y, a, b);
  };
  // Where the shapes of run c..last after column k first paint level y's margin: a busy column, or
  // an edge's corner on its level after; the first such is the leftmost.
  const painter = (k: number, last: number, reach: number, y: number): number => {
    for (let d = k + 1; d <= last; d++) {
      if (state[d] === BUSY) return left + d - pad;
      if (isEdge(d) && after(d) === y) return left + d + at[d] - h;
    }
    return reach;
  };
  // How far the shapes of the run from column c up to column k paint level y's margin, the line
  // having come to c at `pen`. Nothing a column left of d paints reaches past d's end by more than
  // half the line.
  const painted = (c: number, k: number, pen: number | null, y: number): number => {
    let end = -Infinity;
    for (let d = k; d >= c && left + d + 1 + h > end; d--) {
      const x = left + d;
      if (state[d] === BUSY) end = Math.max(end, x + 1 + pad);
      else if (flatAt(d, y)) end = Math.max(end, x + 1);
      else if (isEdge(d) && after(d) === y) end = Math.max(end, x + 1);
      else if (isEdge(d)) {
        // Its line to the edge, and the join's corner past it, which a canvas leaves out where the
        // line starts at the edge itself.
        const joined = at[d] > 0 || (d > c ? state[d - 1] !== BUSY : pen === before(c));
        end = Math.max(end, x + at[d] + (joined ? h : 0));
      }
    }
    return end;
  };
  let pen: number | null = null;
  for (let c = 0; c < count; ) {
    const st = state[c];
    if (st === NONE) {
      pen = null;
      c++;
      continue;
    }
    if (st === LOW || st === HIGH) {
      const y = st === HIGH ? yH : yL;
      const x = left + c;
      if (pen !== y) line.moveTo(x, y);
      line.lineTo(x + 1, y);
      pen = y;
      c++;
      continue;
    }
    // A run of changing columns: those after this one that paint where the run already reaches.
    // Without `join`, only neighbouring busy columns, as the fill always drew them.
    let last = c;
    let reach = hi(c);
    for (let d = c + 1; d < count; d++) {
      if (join) {
        // No later column paints left of its own start by more than half the line. A column with
        // no data ends the run: the line stops and starts again either side of it.
        if (left + d - lw / 2 > reach || state[d] === NONE) break;
        if (!changes(state[d])) continue;
        if (lo(d) > reach) break;
      } else if (st !== BUSY || state[d] !== BUSY) break;
      last = d;
      reach = Math.max(reach, hi(d));
    }
    if (last === c && st !== BUSY) {
      // A lone edge: the line runs to it, along it, and on to the column's end.
      const x = left + c;
      const ex = x + at[c];
      if (pen !== before(c)) line.moveTo(x, before(c));
      line.lineTo(ex, before(c));
      line.lineTo(ex, after(c));
      line.lineTo(x + 1, after(c));
      pen = after(c);
      c++;
      continue;
    }
    const x0 = lo(c);
    bare.length = 0;
    if (st !== BUSY) {
      if (pen === before(c)) {
        // The line comes in along the level: back through the flat columns at it, to where it
        // started or to the edge before them, whose join's corner it reaches.
        let k = c - 1;
        while (k >= 0 && flatAt(k, before(c))) k--;
        gap(before(c), x0, Math.min(k >= 0 && isEdge(k) ? left + k + at[k] - h : left + k + 1, painter(c, last, reach, before(c))));
      } else {
        // The line starts at the first column: its level from there, or, where the edge is at the
        // column's very start, from the next place that paints it (a canvas drops the zero-length
        // segment, and with it the join's corner).
        gap(before(c), x0, Math.min(at[c] > 0 ? left + c : reach, painter(c, last, reach, before(c))));
      }
    }
    if (state[last] !== BUSY) {
      // The line goes on along the level: through the flat columns at it, to where it stops or to
      // the edge after them, whose join's corner it reaches.
      let k = last + 1;
      while (k < count && flatAt(k, after(last))) k++;
      gap(after(last), Math.max(k < count && isEdge(k) ? left + k + at[k] + h : left + k, painted(c, last, pen, after(last))), reach);
    }
    // An edge at its column's very start after a busy column: its level bare from the busy
    // column's end to the next place that paints it.
    for (let k = c + 1; k <= last; k++) {
      if (isEdge(k) && at[k] === 0 && state[k - 1] === BUSY) gap(before(k), painted(c, k - 1, pen, before(k)), painter(k, last, reach, before(k)));
    }
    if (bare.length === 0) fill.rect(x0, top, reach - x0, yL + h - top);
    else {
      // The band between the levels whole, with any margin that has no bare place; each other
      // margin less its bare places.
      let [above, below] = [false, false];
      for (let i = 0; i < bare.length; i += 3) [above, below] = [above || bare[i] === yH, below || bare[i] === yL];
      fill.rect(x0, above ? yH : top, reach - x0, (below ? yL : yL + h) - (above ? yH : top));
      for (const [y, y0, has] of [
        [yH, top, above],
        [yL, yL, below],
      ] as const) {
        if (!has) continue;
        const places: [number, number][] = [];
        for (let i = 0; i < bare.length; i += 3) if (bare[i] === y) places.push([bare[i + 1], bare[i + 2]]);
        let x = x0;
        for (const [a, b] of places.sort((u, v) => u[0] - v[0])) {
          if (a > x) fill.rect(x, y0, a - x, h);
          x = Math.max(x, b);
        }
        if (reach > x) fill.rect(x, y0, reach - x, h);
      }
    }
    // The line meets the run at its first edge and leaves from its last; a busy column ends it.
    if (st !== BUSY) {
      if (pen !== before(c)) line.moveTo(left + c, before(c));
      line.lineTo(left + c + at[c], before(c));
    }
    pen = null;
    if (state[last] !== BUSY) {
      line.moveTo(left + last + at[last], after(last));
      line.lineTo(left + last + 1, after(last));
      pen = after(last);
    }
    c = last + 1;
  }
}
