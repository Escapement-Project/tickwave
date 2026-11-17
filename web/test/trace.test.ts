// A digital trace's shapes, each column drawn as its look says (shade.ts), measured by the opacity
// they paint: the line at full ink, each fill at its alpha. A canvas paints a stroked horizontal
// as its butt-capped rectangle, and a stroked vertical between two horizontals as its rectangle
// with the miter joins' corners. Edges that stand apart at full ink are checked against the
// drawing before the model, every edge stroked, kept here as the reference.

import { test } from "node:test";
import assert from "node:assert/strict";
import { BUSY, digitalFromEdges, FALL, HIGH, LOW, NONE, RISE, type DigitalColumns } from "../src/render/columns.ts";
import { FLAT, HELD, lookAt, looks, PULSE, SHADE, STROKE, type Look, type Looks } from "../src/render/shade.ts";
import { digitalTrace, shadeStep, SHADES, type Fills, type Pen, type TraceBand } from "../src/render/trace.ts";

type Op = ["M" | "L", number, number];

/** A pen that keeps what it is told. */
function recorder(): Pen & { ops: Op[] } {
  const ops: Op[] = [];
  return {
    ops,
    moveTo: (x, y) => void ops.push(["M", x, y]),
    lineTo: (x, y) => void ops.push(["L", x, y]),
  };
}

/** `looks`' arrays holding `list`. */
function looksOf(list: readonly Look[]): Looks {
  const n = list.length;
  const l = { kind: new Uint8Array(n), band: new Float64Array(n), high: new Float64Array(n), low: new Float64Array(n), at: new Float64Array(n) };
  list.forEach((x, k) => {
    if (x === "flat") l.kind[k] = FLAT;
    else if (x[0] === "stroke") [l.kind[k], l.band[k]] = [STROKE, x[1]];
    else if (x[0] === "pulse") [l.kind[k], l.band[k], l.at[k]] = [PULSE, x[1], x[2]];
    else if (x[0] === "held") [l.kind[k], l.band[k]] = [HELD, x[1]];
    else [l.kind[k], l.band[k], l.high[k], l.low[k]] = [SHADE, x[1], x[2], x[3]];
  });
  return l;
}

/** What a trace draws: its line, and its fills. */
function draw(cols: DigitalColumns, look: readonly Look[] | Looks, band: TraceBand): { line: Op[]; fills: Fills | null } {
  const line = recorder();
  const fills = digitalTrace(cols, Array.isArray(look) ? looksOf(look) : (look as Looks), cols.state.length, band, line);
  return { line: line.ops, fills };
}

/** Each layer's alphas, the band's and each rail's, null for no fill. */
const alphas = (f: Fills | null) => f && [[...f.band], [...f.high], [...f.low]];

/** `ops` with each lineTo that carries on along a horizontal joined to it, as a trace gives a
 * level held over several columns. */
function joined(ops: Op[]): Op[] {
  const out: Op[] = [];
  for (const op of ops) {
    const [last, before] = [out.at(-1), out.at(-2)];
    if (op[0] === "L" && last?.[0] === "L" && before && before[2] === last[2] && last[2] === op[2]) last[1] = op[1];
    else out.push([...op] as Op);
  }
  return out;
}

/** The drawing before the model, as plot.ts had it: every edge stroked, busy columns filled. */
function strokes(cols: DigitalColumns, count: number, { left, yH, yL }: TraceBand, line: Pen): void {
  let pen: number | null = null;
  for (let c = 0; c < count; c++) {
    const st = cols.state[c];
    const x = left + c;
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
    } else pen = null;
  }
}

type Rect = [number, number, number, number];

/** The rectangles a canvas paints for a stroked line of width `lw`: each segment's own
 * rectangle, butt-capped, and at each join only the outer corner's square, as a miter fills it at
 * a right angle. A zero-length segment is dropped, as a canvas drops it. A trace's line has
 * horizontal and vertical segments only. */
function painted(line: Op[], lw: number): Rect[] {
  const h = lw / 2;
  const out: Rect[] = [];
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

/** The opacity a drawing paints at (x, y): full where the line does, else its fill's there: the
 * band's between the levels, and each rail's a line's width deep about its level. */
function opacity(d: { line: Op[]; fills: Fills | null }, band: TraceBand, x: number, y: number): number {
  const { left, yH, yL, lw } = band;
  const inside = ([a, b, c, e]: Rect) => a <= x && x < c && b <= y && y < e;
  if (painted(d.line, lw).some(inside)) return 1;
  const c = Math.floor(x - left);
  if (!d.fills || c < 0 || c >= d.fills.band.length) return 0;
  const h = lw / 2;
  const layers = [
    [d.fills.band, yH + h, yL - h],
    [d.fills.high, yH - h, yH + h],
    [d.fills.low, yL - h, yL + h],
  ] as const;
  for (const [alpha, top, bottom] of layers) if (top <= y && y < bottom) return alpha[c] / 255;
  return 0;
}

/** Columns from edges at random ticks, `per` ticks a column, from a seed: `rate` is the chance a
 * tick holds an edge, and a stretch with no data now and then. */
function seeded(seed: number, count: number, per: number, rate: number): DigitalColumns {
  let s = seed;
  const next = () => (s = (s * 1103515245 + 12345) % 2 ** 31) / 2 ** 31;
  const ticks: number[] = [];
  // Bursts: a stretch dense with edges, then a quiet one, so runs and lone edges both come.
  for (let t = 0; t < count * per; t++) {
    const burst = Math.floor(t / (per * 40)) % 3 === 0;
    if (next() < (burst ? rate : rate / 200)) ticks.push(t);
  }
  const cols = digitalFromEdges({ x0: 0, per, count }, { level: 0, ticks: Float64Array.from(ticks) }, 0, count * per);
  // A few columns with no data, each ending what was drawn before it.
  for (let c = 0; c < count; c++) if (next() < 0.01) cols.state[c] = NONE;
  return cols;
}

const TIMEBASE = { num: 1e9, den: 1 };

test("the model paints what a canvas paints: butt caps, and at a join only the outer corner", () => {
  // A rise at a quarter of a column 2 px wide, after a column with no data: the line starts at the
  // column's start, runs a quarter along the low level, up, and on along the high one.
  const line: Op[] = [["M", 1, 30], ["L", 1.25, 30], ["L", 1.25, 6], ["L", 3, 6]];
  const rects = painted(line, 2);
  const covers = (x: number, y: number) => rects.some(([a, b, c, d]) => a <= x && x < c && b <= y && y < d);
  // Left of the column's start, under the low level: the vertical's stroke stops at the level, and
  // no segment runs there. A model with full squares at the vertical's ends paints it.
  assert.equal(covers(0.5, 30.5), false);
  // The join's outer corners: under the low level right of the edge, over the high level left of it.
  assert.equal(covers(1.75, 30.5), true);
  assert.equal(covers(0.5, 5.5), true);
  // Over the high level right of the edge, the horizontal; under it, inside the turn, nothing more.
  assert.equal(covers(2.5, 5.5), true);
});

test("edges that stand apart at full ink are the strokes they were, and nothing is filled", () => {
  const band: TraceBand = { left: 0, yH: 6.5, yL: 30.5, lw: 3 };
  // Edges ten columns apart, each at a quarter of its column.
  const n = 100;
  const state = Uint8Array.from({ length: n }, (_, c) => (c % 10 === 5 ? (c % 20 === 5 ? RISE : FALL) : c % 20 < 5 || c % 20 > 15 ? LOW : HIGH));
  const cols: DigitalColumns = { state, at: new Float64Array(n).fill(0.25), edges: new Float64Array(n), high: new Float64Array(n), known: new Float64Array(n) };
  const look: Look[] = [...state].map((s) => (s === RISE || s === FALL ? ["stroke", 1] : "flat"));
  const old = recorder();
  strokes(cols, n, band, old);
  const now = draw(cols, look, band);
  assert.deepEqual(now.line, joined(old.ops));
  assert.equal(now.fills, null);
});

test("an edge the model fades keeps its levels on the line, and crosses the band at its opacity", () => {
  const band: TraceBand = { left: 10, yH: 6, yL: 30, lw: 2 };
  const cols: DigitalColumns = {
    state: Uint8Array.from([LOW, RISE, HIGH]),
    at: Float64Array.from([0, 0.25, 0]),
    edges: new Float64Array(3),
    high: new Float64Array(3),
    known: new Float64Array(3),
  };
  const d = draw(cols, ["flat", ["stroke", 0.5], "flat"], band);
  assert.deepEqual(d.line, [
    ["M", 10, 30],
    ["L", 11.25, 30],
    ["M", 11.25, 6],
    ["L", 13, 6],
  ]);
  // The stroke from 10.25 to 12.25 at half ink, 255 × 0.5 by the share of each column it covers:
  // three quarters of column 0, all of 1, a quarter of 2, across the band and both rails.
  assert.deepEqual(alphas(d.fills), [
    [96, 128, 32],
    [96, 128, 32],
    [96, 128, 32],
  ]);
  // At full ink the same edge is the line's, and nothing is filled.
  const full = draw(cols, ["flat", ["stroke", 1], "flat"], band);
  assert.deepEqual(full.line, [
    ["M", 10, 30],
    ["L", 11.25, 30],
    ["L", 11.25, 6],
    ["L", 13, 6],
  ]);
  assert.equal(full.fills, null);
});

test("a busy column alone is a pulse at its middle, between the levels either side of it", () => {
  const band: TraceBand = { left: 0, yH: 6, yL: 30, lw: 2 };
  const cols: DigitalColumns = {
    state: Uint8Array.from([HIGH, BUSY, LOW]),
    at: new Float64Array(3),
    edges: Float64Array.from([0, 3, 0]),
    high: new Float64Array(3),
    known: new Float64Array(3),
  };
  const d = draw(cols, ["flat", ["pulse", 0.75, 1.5], "flat"], band);
  assert.deepEqual(d.line, [
    ["M", 0, 6],
    ["L", 1.5, 6],
    ["M", 1.5, 30],
    ["L", 3, 30],
  ]);
  // From 0.5 to 2.5 at three quarters ink: half of column 0, all of 1, half of 2.
  assert.deepEqual(alphas(d.fills), [
    [96, 191, 96],
    [96, 191, 96],
    [96, 191, 96],
  ]);
});

test("a short run is one pulse at its centre: in at the level before its first column, out at the level after its last", () => {
  const band: TraceBand = { left: 0, yH: 6, yL: 30, lw: 2 };
  // A rise at 1.75 and a fall at 2.25, a pulse centred on 2.0, then low.
  const cols: DigitalColumns = {
    state: Uint8Array.from([LOW, RISE, FALL, LOW, LOW]),
    at: Float64Array.from([0, 0.75, 0.25, 0, 0]),
    edges: Float64Array.from([0, 1, 1, 0, 0]),
    high: new Float64Array(5),
    known: new Float64Array(5),
  };
  const d = draw(cols, ["flat", ["pulse", 1, 2], ["pulse", 1, 2], "flat", "flat"], band);
  // Low to the centre, the stroke, then low from the centre on: the level between its edges, high,
  // drawn nowhere, as the pulse is one mark.
  assert.deepEqual(d.line, [
    ["M", 0, 30],
    ["L", 2, 30],
    ["M", 2, 30],
    ["L", 5, 30],
  ]);
  // From 1 to 3 at full ink: all of columns 1 and 2.
  assert.deepEqual(alphas(d.fills), [
    [0, 255, 255, 0, 0],
    [0, 255, 255, 0, 0],
    [0, 255, 255, 0, 0],
  ]);
  // A pulse whose last column rises, before a column with no data, leaves high.
  const rising: DigitalColumns = {
    state: Uint8Array.from([HIGH, FALL, RISE, NONE]),
    at: Float64Array.from([0, 0.75, 0.25, 0]),
    edges: Float64Array.from([0, 1, 1, 0]),
    high: new Float64Array(4),
    known: new Float64Array(4),
  };
  const up = draw(rising, ["flat", ["pulse", 1, 2], ["pulse", 1, 2], "flat"], band);
  assert.deepEqual(up.line, [
    ["M", 0, 6],
    ["L", 2, 6],
    ["M", 2, 6],
    ["L", 3, 6],
  ]);
  // A fall, a rise and a fall, before a column with no data: in high, out low, from the edges'
  // own levels, though the line had none to carry in and the column after says nothing.
  const three: DigitalColumns = {
    state: Uint8Array.from([FALL, RISE, FALL, NONE]),
    at: Float64Array.from([0.5, 0.5, 0.5, 0]),
    edges: Float64Array.from([1, 1, 1, 0]),
    high: new Float64Array(4),
    known: new Float64Array(4),
  };
  const pulse: Look = ["pulse", 1, 1.5];
  const down = draw(three, [pulse, pulse, pulse, "flat"], band);
  assert.deepEqual(down.line, [
    ["M", 0, 6],
    ["L", 1.5, 6],
    ["M", 1.5, 30],
    ["L", 3, 30],
  ]);
});

test("a run is its band and its rails at their opacities, column by column, and the line stops at its sides", () => {
  const band: TraceBand = { left: 0, yH: 6, yL: 30, lw: 2 };
  const cols: DigitalColumns = {
    state: Uint8Array.from([LOW, RISE, FALL, RISE, HIGH, FALL, LOW]),
    at: Float64Array.from([0, 0.5, 0.5, 0.5, 0, 0.5, 0]),
    edges: new Float64Array(7),
    high: new Float64Array(7),
    known: new Float64Array(7),
  };
  const shade = (b: number, h: number, l: number): Look => ["shade", b, h, l];
  const look: Look[] = ["flat", shade(0.5, 0.25, 0.75), shade(0.5, 0.25, 0.75), shade(0.5, 0.5, 0.5), shade(0.5, 0.5, 0.5), shade(0.5, 0.5, 0.5), "flat"];
  const d = draw(cols, look, band);
  assert.deepEqual(d.line, [
    ["M", 0, 30],
    ["L", 1, 30],
    ["M", 6, 30],
    ["L", 7, 30],
  ]);
  // The band whole across the run at half ink, and each rail at its column's: a quarter, half,
  // three quarters, each 255 × its step of 64, rounded.
  assert.deepEqual(alphas(d.fills), [
    [0, 128, 128, 128, 128, 128, 0],
    [0, 64, 64, 128, 128, 128, 0],
    [0, 191, 191, 128, 128, 128, 0],
  ]);
});

test("a level held briefly between two runs keeps its gap: no band, its line at the look's opacity, and none of the line's ink", () => {
  const band: TraceBand = { left: 0, yH: 6, yL: 30, lw: 2 };
  const cols: DigitalColumns = {
    state: Uint8Array.from([LOW, RISE, FALL, RISE, HIGH, HIGH, FALL, RISE, FALL, LOW]),
    at: Float64Array.from([0, 0.5, 0.5, 0.5, 0, 0, 0.5, 0.5, 0.5, 0]),
    edges: new Float64Array(10),
    high: new Float64Array(10),
    known: new Float64Array(10),
  };
  const run: Look = ["shade", 0.5, 0.5, 0.5];
  const look: Look[] = ["flat", run, run, run, ["held", 0.25], ["held", 0.25], run, run, run, "flat"];
  const d = draw(cols, look, band);
  // The line stops at the first run's side and starts again after the second.
  assert.deepEqual(d.line, [
    ["M", 0, 30],
    ["L", 1, 30],
    ["M", 9, 30],
    ["L", 10, 30],
  ]);
  // Columns 4 and 5: no band, the high level's rail at a quarter, 255 × 16 / 64, and no low rail.
  assert.deepEqual(alphas(d.fills), [
    [0, 128, 128, 128, 0, 0, 128, 128, 128, 0],
    [0, 128, 128, 128, 64, 64, 128, 128, 128, 0],
    [0, 128, 128, 128, 0, 0, 128, 128, 128, 0],
  ]);
});

test("each column paints as its look says: a run's band and rails at their steps, a flat column's level at full ink, a pulse's stroke at its step", () => {
  let [cases, pulses] = [0, 0];
  for (const lw of [1, 2, 3, 4]) {
    const band: TraceBand = { left: 5, yH: lw % 2 ? 6.5 : 6, yL: lw % 2 ? 30.5 : 30, lw };
    for (const rate of [0.002, 0.01, 0.05, 0.3]) {
      for (let seed = 1; seed <= 6; seed++) {
        const count = 240;
        const cols = seeded(seed * 7919 + lw, count, 100, rate);
        const look = looks(cols, 0, count, lw, 100, TIMEBASE);
        const d = draw(cols, look, band);
        const mid = (band.yH + band.yL) / 2;
        const kinds = new Set<string>();
        for (let c = 0; c < count; c++) {
          const x = band.left + c + 0.5;
          const l = lookAt(look, c);
          const at = (y: number) => opacity(d, band, x, y);
          if (l !== "flat" && l[0] === "shade") {
            kinds.add("shade");
            // Each at its step, as the 8-bit alpha a canvas holds.
            const [b, h, low] = [l[1], l[2], l[3]].map((a) => Math.round((255 * shadeStep(a)) / SHADES) / 255);
            assert.deepEqual([at(mid), at(band.yH), at(band.yL)], [b, h, low], `lw ${lw}, rate ${rate}, seed ${seed}, column ${c}`);
          } else if (l === "flat" && (cols.state[c] === LOW || cols.state[c] === HIGH)) {
            kinds.add("flat");
            assert.equal(at(cols.state[c] === HIGH ? band.yH : band.yL), 1, `lw ${lw}, rate ${rate}, seed ${seed}, column ${c}`);
          } else if (l !== "flat" && l[0] === "held") {
            kinds.add("held");
            // Its level at its step, the band empty.
            const q = Math.round((255 * shadeStep(l[1])) / SHADES) / 255;
            assert.deepEqual([at(cols.state[c] === HIGH ? band.yH : band.yL), at(mid)], [q, 0], `lw ${lw}, rate ${rate}, seed ${seed}, column ${c}`);
          } else if (l !== "flat" && l[0] === "stroke") kinds.add("stroke");
          else if (l !== "flat" && l[0] === "pulse") {
            kinds.add("pulse");
            pulses++;
            // The column under its stroke's centre, across the band at its step, where the stroke
            // covers that column whole.
            const under = Math.floor(l[2]);
            if (lw >= 2 && l[2] - under >= 0.5 - (lw - 1) / 2 && l[2] - under <= 0.5 + (lw - 1) / 2) {
              const want = Math.round((255 * shadeStep(l[1])) / SHADES) / 255;
              assert.equal(opacity(d, band, band.left + under + 0.5, mid), want, `lw ${lw}, rate ${rate}, seed ${seed}, column ${c}`);
            }
          }
        }
        // Not vacuous: each sweep's draws hold runs, flat columns and lone strokes.
        if (rate >= 0.01) for (const k of ["flat", "shade", "stroke"]) assert.ok(kinds.has(k), `lw ${lw}, rate ${rate}, seed ${seed}: no ${k}`);
        cases++;
      }
    }
  }
  assert.equal(cases, 96);
  assert.ok(pulses > 0, "no pulse in the sweep");
});
