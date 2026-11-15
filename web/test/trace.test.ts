// A digital trace's shapes against the strokes it replaces: the old drawing (every edge stroked,
// busy columns filled) is kept here as the reference, and both are measured by the area they
// cover, sampled finely, from the shapes a canvas paints: a stroked horizontal is its butt-capped
// rectangle, a stroked vertical between two horizontals its rectangle with the miter joins' corners.

import { test } from "node:test";
import assert from "node:assert/strict";
import { BUSY, FALL, HIGH, LOW, NONE, RISE, type DigitalColumns } from "../src/render/columns.ts";
import { digitalTrace, type Pen, type TraceBand } from "../src/render/trace.ts";

type Op = ["M" | "L", number, number] | ["R", number, number, number, number];

/** A pen that keeps what it is told. */
function recorder(): Pen & { ops: Op[] } {
  const ops: Op[] = [];
  return {
    ops,
    moveTo: (x, y) => void ops.push(["M", x, y]),
    lineTo: (x, y) => void ops.push(["L", x, y]),
    rect: (x, y, w, h) => void ops.push(["R", x, y, w, h]),
  };
}

/** The drawing before the runs were joined, as plot.ts had it. */
function strokes(cols: DigitalColumns, count: number, { left, yH, yL, lw }: TraceBand, line: Pen, fill: Pen): void {
  let pen: number | null = null;
  let busyFrom = -1;
  const pad = (lw - 1) / 2;
  const flushBusy = (c: number) => {
    if (busyFrom < 0) return;
    fill.rect(left + busyFrom - pad, yH - lw / 2, c - busyFrom + 2 * pad, yL - yH + lw);
    busyFrom = -1;
  };
  for (let c = 0; c < count; c++) {
    const st = cols.state[c];
    const x = left + c;
    if (st !== BUSY) flushBusy(c);
    if (st === NONE) {
      pen = null;
    } else if (st === LOW || st === HIGH) {
      const y = st === HIGH ? yH : yL;
      if (pen !== y) line.moveTo(x, y);
      line.lineTo(x + 1, y);
      pen = y;
    } else if (st === RISE || st === FALL) {
      const ya = st === RISE ? yL : yH;
      const yb = st === RISE ? yH : yL;
      const ex = x + cols.at[c];
      if (pen !== ya) line.moveTo(x, ya);
      line.lineTo(ex, ya);
      line.lineTo(ex, yb);
      line.lineTo(x + 1, yb);
      pen = yb;
    } else {
      if (busyFrom < 0) busyFrom = c;
      pen = null;
    }
  }
  flushBusy(count);
}

type Rect = [number, number, number, number];

/** The rectangles a canvas paints for a stroked line of width `lw` and a fill: each segment's own
 * rectangle, butt-capped, and at each join only the outer corner's square, as a miter fills it at
 * a right angle. A zero-length segment is dropped, as a canvas drops it. A trace's line has
 * horizontal and vertical segments only. */
function painted(line: Op[], fill: Op[], lw: number): Rect[] {
  const h = lw / 2;
  const out: Rect[] = [];
  for (const op of fill) if (op[0] === "R") out.push([op[1], op[2], op[1] + op[3], op[2] + op[4]]);
  let points: [number, number][] = [];
  const flush = () => {
    const p = points.filter((q, i) => i === 0 || q[0] !== points[i - 1][0] || q[1] !== points[i - 1][1]);
    const dir = (a: [number, number], b: [number, number]) => [Math.sign(b[0] - a[0]), Math.sign(b[1] - a[1])];
    for (let i = 0; i + 1 < p.length; i++) {
      const [[x0, y0], [x1, y1]] = [p[i], p[i + 1]];
      assert.ok(x0 === x1 || y0 === y1, `a segment neither horizontal nor vertical: ${p[i]} to ${p[i + 1]}`);
      if (y0 === y1) out.push([Math.min(x0, x1), y0 - h, Math.max(x0, x1), y0 + h]);
      else out.push([x0 - h, Math.min(y0, y1), x0 + h, Math.max(y0, y1)]);
    }
    for (let i = 1; i + 1 < p.length; i++) {
      const [d1, d2] = [dir(p[i - 1], p[i]), dir(p[i], p[i + 1])];
      if (d1[0] === d2[0] && d1[1] === d2[1]) continue;
      // The outer corner: from the vertex on along the way in, and back against the way out.
      const [x, y] = p[i];
      const [cx, cy] = [x + h * (d1[0] - d2[0]), y + h * (d1[1] - d2[1])];
      out.push([Math.min(x, cx), Math.min(y, cy), Math.max(x, cx), Math.max(y, cy)]);
    }
    points = [];
  };
  for (const op of line) {
    if (op[0] === "M") {
      flush();
      points = [[op[1], op[2]]];
    } else if (op[0] === "L") points.push([op[1], op[2]]);
  }
  flush();
  return out;
}

// Every shape's side lies on a multiple of 1/32 across and of 1/2 down (the cases' edges sit on a
// 1/32 grid, the levels and the line's half-width on halves), so samples between those multiples
// tell any two areas apart and never fall on a side.
const SX = 1 / 32;
const SY = 1 / 2;

/** Which points of the grid over the band the rectangles cover. */
function coverage(rects: Rect[], band: TraceBand, count: number): Uint8Array {
  const x0 = band.left - band.lw - 1;
  const x1 = band.left + count + band.lw + 1;
  const y0 = band.yH - band.lw - 1;
  const y1 = band.yL + band.lw + 1;
  const nx = Math.ceil((x1 - x0) / SX);
  const ny = Math.ceil((y1 - y0) / SY);
  const out = new Uint8Array(nx * ny);
  // Sample i sits at x0 + (i + 1/2) SX: inside [a, c) for i from ceil((a - x0) / SX - 1/2).
  const from = (v: number, o: number, step: number, n: number) => Math.min(n, Math.max(0, Math.ceil((v - o) / step - 0.5)));
  for (const [a, b, c, d] of rects) {
    const [i0, i1] = [from(a, x0, SX, nx), from(c, x0, SX, nx)];
    for (let j = from(b, y0, SY, ny); j < from(d, y0, SY, ny); j++) out.fill(1, j * nx + i0, j * nx + i1);
  }
  return out;
}

/** Each column's state and edge position, from a seed: `dense` is the chance a column changes. */
function columns(seed: number, count: number, dense: number, busy: number): DigitalColumns {
  let s = seed;
  const next = () => ((s = (s * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);
  const state = new Uint8Array(count);
  const at = new Float64Array(count);
  let level = 0;
  for (let c = 0; c < count; c++) {
    const r = next();
    if (r < 0.03) state[c] = NONE;
    else if (r < 0.03 + dense * busy) state[c] = BUSY;
    else if (r < 0.03 + dense) {
      state[c] = level ? FALL : RISE;
      // On a 1/32 grid, so no edge falls between the samples' grid and its neighbour's.
      at[c] = Math.floor(next() * 32) / 32;
      level ^= 1;
    } else state[c] = level ? HIGH : LOW;
  }
  return { state, at };
}

function both(cols: DigitalColumns, count: number, band: TraceBand, join: boolean) {
  const [ol, of] = [recorder(), recorder()];
  strokes(cols, count, band, ol, of);
  const [nl, nf] = [recorder(), recorder()];
  digitalTrace(cols, count, band, join, nl, nf);
  return { old: { line: ol.ops, fill: of.ops }, now: { line: nl.ops, fill: nf.ops } };
}

const firstDifference = (a: Uint8Array, b: Uint8Array) => a.findIndex((v, i) => v !== b[i]);

/** Where sample `i` of `coverage`'s grid lies, which of the two covers it, and the columns about it. */
function where(i: number, band: TraceBand, count: number, cols: DigitalColumns, strokes: Uint8Array): string {
  const x0 = band.left - band.lw - 1;
  const nx = Math.ceil((band.left + count + band.lw + 1 - x0) / SX);
  const [x, y] = [x0 + ((i % nx) + 0.5) * SX, band.yH - band.lw - 1 + (Math.floor(i / nx) + 0.5) * SY];
  const c = Math.floor(x - band.left);
  const near = [c - 1, c, c + 1].filter((k) => k >= 0 && k < count).map((k) => `${k}:${["NONE", "LOW", "HIGH", "BUSY", "RISE", "FALL"][cols.state[k]]}@${cols.at[k]}`);
  return `(${x}, ${y}), covered by ${strokes[i] ? "the strokes" : "the joined shapes"} alone; columns ${near.join(" ")}`;
}

test("the model paints what a canvas paints: butt caps, and at a join only the outer corner", () => {
  // A rise at a quarter of a column 2 px wide, after a column with no data: the line starts at the
  // column's start, runs a quarter along the low level, up, and on along the high one.
  const line: Op[] = [["M", 1, 30], ["L", 1.25, 30], ["L", 1.25, 6], ["L", 3, 6]];
  const rects = painted(line, [], 2);
  const covers = (x: number, y: number) => rects.some(([a, b, c, d]) => a <= x && x < c && b <= y && y < d);
  // Left of the column's start, under the low level: the vertical's stroke stops at the level, and
  // no segment runs there. A model with full squares at the vertical's ends paints it.
  assert.equal(covers(0.5, 30.5), false);
  // The join's outer corners: under the low level right of the edge, over the high level left of it.
  assert.equal(covers(1.75, 30.5), true);
  assert.equal(covers(0.5, 5.5), true);
  // Over the high level right of the edge, the horizontal; under it, inside the turn, nothing more.
  assert.equal(covers(2.5, 5.5), true);
  // An edge at its column's very start: the zero-length segment before it goes, and with it the
  // join, so nothing paints the low level's margin either side of it.
  const at0 = painted([["M", 1, 30], ["L", 1, 30], ["L", 1, 6], ["L", 3, 6]], [], 2);
  assert.equal(at0.some(([a, b, c, d]) => a <= 1.5 && 1.5 < c && b <= 30.5 && 30.5 < d), false);
});

test("beside a column with no data, a run paints the margins the strokes left bare, at every width", () => {
  const cases: [string, number[], number[]][] = [
    // Edges a column apart starting a quarter in: the line starts and stops at the run's columns,
    // inside the run's sides.
    ["edges", [NONE, RISE, FALL, RISE, FALL, NONE], [0, 0.25, 0.5, 0.5, 0.75, 0]],
    // A busy column, then an edge at its column's very start: no line runs to that edge, so a
    // canvas paints no corner there, and the level is bare from the busy column's end.
    ["busy, then an edge at its start", [NONE, BUSY, RISE, FALL, NONE], [0, 0, 0, 0.5, 0]],
  ];
  for (const [name, s, t] of cases) {
    for (const lw of [2, 3, 4, 5, 6]) {
      const band: TraceBand = { left: 10, yH: lw % 2 ? 6.5 : 6, yL: lw % 2 ? 30.5 : 30, lw };
      const cols = { state: Uint8Array.from(s), at: Float64Array.from(t) };
      const { old, now } = both(cols, s.length, band, true);
      assert.ok(now.fill.length > 1, `${name}, lw ${lw}: one rectangle`);
      const a = coverage(painted(old.line, old.fill, lw), band, s.length);
      const b = coverage(painted(now.line, now.fill, lw), band, s.length);
      const i = firstDifference(a, b);
      assert.equal(i, -1, `${name}, lw ${lw}: differs at ${i < 0 ? "" : where(i, band, s.length, cols, a)}`);
    }
  }
});

test("joined runs cover exactly what the strokes covered, at every line width and density", () => {
  let cases = 0;
  // Widths up to 6: a heavier stretch, or a third device pixel to the CSS pixel.
  for (const lw of [1, 2, 3, 4, 5, 6]) {
    // yH and yL as the plot snaps them: on a half pixel for an odd width.
    const band: TraceBand = { left: 10, yH: lw % 2 ? 6.5 : 6, yL: lw % 2 ? 30.5 : 30, lw };
    for (const [dense, busy] of [[0.1, 0], [0.5, 0.1], [0.8, 0.3], [1, 0], [0.97, 0.5]]) {
      for (let seed = 1; seed <= 12; seed++) {
        const count = 300;
        const cols = columns(seed * 7919 + lw, count, dense, busy);
        const { old, now } = both(cols, count, band, true);
        const a = coverage(painted(old.line, old.fill, lw), band, count);
        const b = coverage(painted(now.line, now.fill, lw), band, count);
        const i = firstDifference(a, b);
        assert.equal(i, -1, `lw ${lw}, density ${dense}, seed ${seed}: differs at ${i < 0 ? "" : where(i, band, count, cols, a)}`);
        cases++;
      }
    }
  }
  assert.equal(cases, 360);
});

test("a run of edges a column apart is one shape; lone edges keep their strokes", () => {
  const band: TraceBand = { left: 0, yH: 6.5, yL: 30.5, lw: 3 };
  // 1,000 columns, an edge in each: the strokes overlap all the way along.
  const n = 1000;
  const dense: DigitalColumns = { state: Uint8Array.from({ length: n }, (_, c) => (c % 2 ? FALL : RISE)), at: new Float64Array(n).fill(0.5) };
  const { old, now } = both(dense, n, band, true);
  assert.equal(old.line.length, 3 * n + 1);
  // The band and the margin above whole; the margin below, which the line comes in on from the
  // row's start and leaves on at its end, only from the first column's start to the last's end.
  assert.deepEqual(now.fill, [
    ["R", 0.5 - 1.5, 6.5 - 1.5, n - 1 + 3, 24 + 1.5],
    ["R", 0, 30.5, n, 1.5],
  ]);
  assert.deepEqual(now.line, [["M", 0, 30.5], ["L", 0.5, 30.5], ["M", n - 1 + 0.5, 30.5], ["L", n, 30.5]]);
  // Edges ten columns apart: each stroked as before, nothing filled.
  const sparse: DigitalColumns = { state: Uint8Array.from({ length: 100 }, (_, c) => (c % 10 === 5 ? (c % 20 === 5 ? RISE : FALL) : c % 20 < 5 || c % 20 > 15 ? LOW : HIGH)), at: new Float64Array(100).fill(0.25) };
  const s = both(sparse, 100, band, true);
  assert.deepEqual(s.now, s.old);
  assert.deepEqual(s.now.fill, []);
});

test("where the fill is faded, nothing is joined: the shapes are the strokes' own", () => {
  for (const lw of [1, 2, 3]) {
    const band: TraceBand = { left: 4, yH: lw % 2 ? 6.5 : 6, yL: lw % 2 ? 30.5 : 30, lw };
    for (let seed = 1; seed <= 20; seed++) {
      const cols = columns(seed, 200, 0.7, 0.4);
      const { old, now } = both(cols, 200, band, false);
      assert.deepEqual(now, old, `lw ${lw}, seed ${seed}`);
    }
  }
});
