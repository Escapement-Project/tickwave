// An analog row's whole range comes from its base, the coarsest tile it reads (track.ts
// `wholeRange`), not from the recording's head. Until every analog row's base is in, the plot
// draws nothing and keeps the canvas as it was, so no frame shows a names column that then
// widens; once they are in, it says so as the next frame begins (`onRange`), once, and the layout
// that follows is the first it draws. A row whose base came back null (no overview) shows its name
// alone until a read of the whole channel gives it a range.

import { test } from "node:test";
import assert from "node:assert/strict";
import { MemorySource } from "../src/memory-source.ts";
import { Plot } from "../src/render/plot.ts";
import { formatMarks } from "../src/readouts.ts";
import type { Palette } from "../src/palette.ts";

/** A 2D context that takes every call, and records the text it draws. */
function context(texts: string[]): CanvasRenderingContext2D {
  const fns: Record<string, unknown> = {
    measureText: (s: string) => ({ width: 7 * s.length }),
    fillText: (s: string) => void texts.push(s),
  };
  return new Proxy({} as Record<string | symbol, unknown>, {
    get: (o, k) => (k in o ? o[k] : typeof k === "string" && k in fns ? fns[k] : () => undefined),
    set: (o, k, v) => ((o[k] = v), true),
  }) as unknown as CanvasRenderingContext2D;
}

/** Stand-ins for the browser's frames and paths: `frame` runs the frame the plot asked for, if
 * it asked, and says whether it did. */
function browser(t: { after: (f: () => void) => void }): { frame: () => boolean } {
  const g = globalThis as { requestAnimationFrame?: unknown; Path2D?: unknown };
  let asked: (() => void) | null = null;
  g.requestAnimationFrame = (f: () => void) => {
    asked = f;
    return 1;
  };
  g.Path2D = function () {
    return new Proxy({}, { get: () => () => undefined });
  };
  t.after(() => {
    delete g.requestAnimationFrame;
    delete g.Path2D;
  });
  return {
    frame: () => {
      const f = asked;
      asked = null;
      f?.();
      return f !== null;
    },
  };
}

/** A plot over `source`'s channels `shown`, its view the whole recording. */
async function plotOf(source: MemorySource, shown: number[], texts: string[]): Promise<Plot> {
  const plot = new Plot({ getContext: () => context(texts), style: {}, width: 0, height: 0 } as unknown as HTMLCanvasElement, {
    axis: false,
    lanes: 0,
    rowHeight: 26,
    analogHeight: 56,
    noteRules: false,
    labelPx: 11,
  });
  plot.palette = palette;
  const info = await source.info();
  plot.setRecording(source, info, shown);
  plot.view = { start: 0, end: info.length };
  return plot;
}

const palette: Palette = {
  ...Object.fromEntries(["ink", "ink2", "hair", "rule", "page", "wash", "trace"].map((k) => [k, "#000"])),
  fontText: "sans-serif",
  fontFigures: "monospace",
  lineWeight: 1.5,
  role: () => "#000",
} as unknown as Palette;

/** An analog channel of `n` samples every 10 ticks, `f` of the sample's index. */
const analog = (name: string, n: number, f: (i: number) => number) => ({
  kind: "analog" as const,
  name,
  end: 10 * n,
  unit: "V",
  first: 0,
  period: { num: 10, den: 1 },
  resolution: null,
  values: Float64Array.from({ length: n }, (_, i) => f(i)),
});

/** Lays `plot` out as the element does, again whenever it asks (`onRange`), and keeps the names
 * column of every frame it draws. */
function laidOut(plot: Plot): { said: () => number; drawn: number[] } {
  let said = 0;
  const drawn: number[] = [];
  plot.onRange = () => {
    said++;
    plot.resize(800, null);
  };
  plot.onDraw = () => void drawn.push(plot.layout!.plotLeft);
  plot.resize(800, null);
  return { said: () => said, drawn };
}

/** Runs the frames the plot asks for, while `ms` pass. */
async function frames(frame: () => boolean, ms: number): Promise<void> {
  const end = performance.now() + ms;
  while (performance.now() < end) {
    frame();
    await new Promise((r) => setTimeout(r, 1));
  }
  while (frame());
}
test("until an analog row's base is in, nothing is drawn and the canvas keeps its size; then the plot says so once, and the first frame it draws has the row's whole range", async (t) => {
  const { frame } = browser(t);
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 4_000_000, channels: [analog("VDD", 400_000, (i) => 1.2 + Math.sin(i / 50))] },
    { levels: [64, 1024], latency: 5 },
  );
  const texts: string[] = [];
  const plot = await plotOf(source, [0], texts);
  const [vdd] = plot.tracks;
  const { said, drawn } = laidOut(plot);
  const held = plot.layout!;
  assert.equal(held.rows[0].range, undefined);
  assert.equal(plot.wholeRange(vdd), null);
  // Held: the canvas unsized, nothing drawn, however many frames run.
  assert.ok(frame());
  plot.drawNow();
  assert.deepEqual([plot.canvas.style.height, drawn.length, texts.length, said()], [undefined, 0, 0, 0]);

  await new Promise((r) => setTimeout(r, 40));
  const whole = plot.wholeRange(vdd);
  assert.ok(whole);
  // 0.2 V to 2.2 V, out to round steps of 0.5 V.
  assert.deepEqual([whole.lo, whole.hi], [0, 2.5]);
  // Said as the frame begins, not as the base lands. A layout before it, as an annotation file
  // landing makes, holds on: the frame sizes the canvas and draws it.
  assert.equal(said(), 0);
  plot.resize(800, null);
  assert.deepEqual([plot.canvas.style.height, drawn.length], [undefined, 0]);
  texts.length = 0;
  assert.ok(frame());
  assert.equal(said(), 1);
  // Drawn once in that frame.
  assert.equal(drawn.length, 1);
  const l = plot.layout!;
  assert.deepEqual(l.rows[0].range, { lo: whole.lo, hi: whole.hi, marks: { top: whole.hi, bottom: whole.lo, place: whole.place } });
  assert.equal(plot.canvas.style.height, `${l.height}px`);
  // Every frame drawn has the names column the range gives, wider than the name's alone.
  assert.ok(drawn.length > 0 && drawn.every((x) => x === l.plotLeft), `${drawn} against ${l.plotLeft}`);
  assert.ok(l.plotLeft > held.plotLeft, `${l.plotLeft} against ${held.plotLeft}`);
  assert.deepEqual(texts.slice(-3), ["VDD", ...formatMarks(whole.hi, whole.lo, whole.place, "V")]);
  // Laid out with its range, the row is not news again: a tile that lands says nothing.
  plot.view = { start: 10_000, end: 12_000 };
  plot.invalidate();
  await frames(frame, 40);
  assert.deepEqual([vdd.tile?.kind, vdd.tile?.start], ["samples", 8_000]);
  assert.equal(said(), 1);
});

test("shown another analog channel, the plot keeps the canvas and draws nothing until its base is in, then lays out once", async (t) => {
  const { frame } = browser(t);
  // B's labels, in the hundreds of thousands of volts, are wider than A's.
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 400_000, channels: [analog("A", 40_000, (i) => Math.sin(i / 30)), analog("B", 40_000, (i) => 123_456 * Math.sin(i / 70))] },
    { levels: [64, 1024], latency: 5 },
  );
  const texts: string[] = [];
  const plot = await plotOf(source, [0], texts);
  const { said, drawn } = laidOut(plot);
  await frames(frame, 40);
  assert.equal(said(), 1);
  const before = { left: plot.layout!.plotLeft, width: plot.canvas.width, height: plot.canvas.height, style: plot.canvas.style.height };
  const drawnBefore = drawn.length;

  // The element's `channels` change: another set of rows, laid out at once.
  plot.setRecording(source, await source.info(), [0, 1]);
  plot.resize(800, null);
  await frames(frame, 2);
  // Held while B's base is out: the canvas as it was, nothing drawn.
  assert.ok(!plot.tracks[1].baseSettled);
  assert.deepEqual({ left: before.left, width: plot.canvas.width, height: plot.canvas.height, style: plot.canvas.style.height }, before);
  assert.equal(drawn.length, drawnBefore);

  await frames(frame, 40);
  assert.equal(said(), 2);
  const l = plot.layout!;
  assert.ok(l.rows.every((r) => r.range));
  assert.ok(l.plotLeft > before.left, `${l.plotLeft} against ${before.left}`);
  // Every frame drawn since has the names column B's range gives.
  const since = drawn.slice(drawnBefore);
  assert.ok(since.length > 0 && since.every((x) => x === l.plotLeft), `${since} against ${l.plotLeft}`);
  assert.notEqual(plot.canvas.height, before.height);
});

test("a row whose base comes back null, with no overview, shows its name alone, and takes its range once a read covers its channel", async (t) => {
  const { frame } = browser(t);
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 40_000, channels: [analog("VDD", 4_000, (i) => 40 + Math.sin(i / 50))] },
    { levels: [], latency: 5 },
  );
  const texts: string[] = [];
  const plot = await plotOf(source, [0], texts);
  const [vdd] = plot.tracks;
  plot.view = { start: 10_000, end: 12_000 };
  const { said, drawn } = laidOut(plot);
  await frames(frame, 40);
  // The base came back null: laid out once more, drawn, with no range.
  assert.ok(vdd.baseSettled);
  assert.equal(plot.wholeRange(vdd), null);
  assert.equal(said(), 1);
  const named = plot.layout!.plotLeft;
  assert.ok(drawn.length > 0 && drawn.every((x) => x === named));
  assert.equal(plot.layout!.rows[0].range, undefined);

  // The whole channel: its samples tile covers it, and the row's range comes from them.
  plot.view = { start: 0, end: 40_000 };
  plot.invalidate();
  await new Promise((r) => setTimeout(r, 40));
  const whole = plot.wholeRange(vdd);
  assert.ok(whole);
  // A frame from the layout without the range draws the name alone: there is no scale to draw by.
  texts.length = 0;
  plot.drawNow();
  assert.deepEqual(texts, ["VDD"]);
  await frames(frame, 2);
  assert.equal(said(), 2);
  assert.ok(plot.layout!.rows[0].range);
  assert.ok(plot.layout!.plotLeft > named);
});

test("a base that comes back null lets the layout go, though no other read has come", async (t) => {
  const { frame } = browser(t);
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 40_000, channels: [analog("VDD", 4_000, (i) => Math.sin(i / 50))] },
    { levels: [], latency: 5 },
  );
  source.samples = () => new Promise(() => {});
  const texts: string[] = [];
  const plot = await plotOf(source, [0], texts);
  const { said, drawn } = laidOut(plot);
  assert.ok(frame());
  await new Promise((r) => setTimeout(r, 40));
  while (frame());
  assert.ok(plot.tracks[0].baseSettled);
  assert.equal(said(), 1);
  assert.ok(drawn.length > 0);
});

test("a row whose base fails is in, for the plot's layout: it draws, the row its name alone", async (t) => {
  const { frame } = browser(t);
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 40_000, channels: [analog("VDD", 4_000, (i) => Math.sin(i / 50))] },
    { levels: [64, 1024] },
  );
  // The base fails a little later; every other read waits for ever, so only the failure asks
  // for a frame.
  source.overview = async (_c, _s, _e, maxBucket) => {
    if (maxBucket !== Infinity) return new Promise(() => {});
    await new Promise((r) => setTimeout(r, 5));
    throw new Error("no strip");
  };
  source.samples = () => new Promise(() => {});
  const texts: string[] = [];
  const plot = await plotOf(source, [0], texts);
  const errors: unknown[] = [];
  plot.onError = (e) => void errors.push(e);
  const { said, drawn } = laidOut(plot);
  assert.ok(frame());
  await new Promise((r) => setTimeout(r, 40));
  while (frame());
  assert.equal(errors.length, 1);
  assert.equal(said(), 1);
  assert.ok(drawn.length > 0);
  assert.equal(plot.layout!.rows[0].range, undefined);
});

test("a base still out after 5 s puts up the stall notice, though nothing is drawn", async (t) => {
  const { frame } = browser(t);
  let now = 0;
  t.mock.method(performance, "now", () => now);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 40_000, channels: [analog("VDD", 4_000, (i) => Math.sin(i / 50))] },
    { levels: [64, 1024] },
  );
  // The base never comes; the view's tiles come at once.
  const overview = source.overview.bind(source);
  source.overview = (c, s, e, maxBucket, signal) => (maxBucket === Infinity ? new Promise(() => {}) : overview(c, s, e, maxBucket, signal));
  const texts: string[] = [];
  const plot = await plotOf(source, [0], texts);
  const stalls: (number | null)[] = [];
  plot.onStall = (ms) => void stalls.push(ms);
  const { drawn } = laidOut(plot);
  await new Promise((r) => setImmediate(r));
  assert.equal(plot.tracks[0].busy, false);
  assert.ok(frame());
  now = 4_999;
  t.mock.timers.tick(4_999);
  assert.deepEqual(stalls, []);
  now = 5_000;
  t.mock.timers.tick(1);
  assert.deepEqual(stalls, [5_000]);
  assert.equal(drawn.length, 0);
});

test("however many rows' bases land between two frames, the plot says so once, and the layout it asks for takes no frame of its own", async (t) => {
  const { frame } = browser(t);
  const channels = Array.from({ length: 40 }, (_, k) => analog(`A${k}`, 40_000, (i) => k + Math.sin(i / 30)));
  const source = new MemorySource({ title: "a", timebase: { num: 1e6, den: 1 }, length: 400_000, channels }, { levels: [64, 1024], latency: 5 });
  const texts: string[] = [];
  const plot = await plotOf(source, channels.map((_, k) => k), texts);
  const { said } = laidOut(plot);
  assert.ok(frame());
  await new Promise((r) => setTimeout(r, 60));
  assert.ok(plot.tracks.every((t) => plot.wholeRange(t)));
  assert.ok(frame());
  assert.equal(said(), 1);
  assert.ok(plot.layout!.rows.every((r) => r.range));
  // The frame drew with the new layout, and asked for no other.
  assert.equal(frame(), false);
});
