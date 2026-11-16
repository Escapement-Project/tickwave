// The view's point measurements: a pulse around a tick, the edges past what is loaded, the figures
// with their digits, and an analog channel at a pixel. Expected values are worked by hand. The
// pure cases are in testdata/view/probe.json, which the Rust view core's tests read too, with the
// pulses below written out over the edges these tests read; the cases that read a MemorySource
// are here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { MemorySource, type MemoryRecording } from "../src/memory-source.ts";
import {
  analogFromOverview,
  analogFromSamples,
  completePulse,
  denseFigures,
  denseFromEdges,
  denseFromOverview,
  findEdge,
  formatDuty,
  pulseFigures,
  pulseFromEdges,
  type EdgeAt,
} from "../src/probe.ts";
import { inChannel, type AnalogChannel, type Source } from "../src/source.ts";
import { ratio, runTable } from "./tables.ts";

/** An edge as the table writes it: undefined, where the edges in hand don't reach it, is "unread". */
const edge = (e: EdgeAt) => (e === undefined ? "unread" : e);

runTable("probe", {
  pulse_from_edges: (a) => {
    const p = pulseFromEdges({ level: a.edges.level, ticks: Float64Array.from(a.edges.ticks) }, a.from, a.to, a.known, a.tick);
    return { level: p.level, start: edge(p.start), end: edge(p.end), next: edge(p.next) };
  },
  pulse_figures: (a) => {
    const f = pulseFigures(a.pulse, ratio(a.timebase), a.known, a.step);
    return { level: f.level, width: f.width, inverse_width: f.inverseWidth, period: f.period, frequency: f.frequency, duty: f.duty };
  },
  format_duty: (a) => formatDuty(a.high, a.period, a.step),
  analog_from_samples: (a) => {
    const c: AnalogChannel = { kind: "analog", name: "V", end: a.end, unit: "V", first: a.runs.first, period: ratio(a.runs.period), count: a.runs.count, resolution: null, min: 0, max: 1, ...(a.runs.breaks && { breaks: a.runs.breaks }) };
    return analogFromSamples(c, { first: a.samples.first, values: Float64Array.from(a.samples.values) }, a.t0, a.t1, a.tick);
  },
  analog_from_overview: (a) => analogFromOverview({ kind: "analog", bucket: a.overview.bucket, start: a.overview.start, min: Float64Array.from(a.overview.min), max: Float64Array.from(a.overview.max) }, a.t0, a.t1),
});

const NS = { num: 1_000_000_000, den: 1 };

/** A clock, then a long quiet stretch, then three edges: 10 ns high, 15 low, from tick 100. */
function recording(): MemoryRecording {
  const ticks: number[] = [];
  for (let t = 100; t < 100 + 25 * 40; t += 25) ticks.push(t, t + 10);
  ticks.push(50_000_000, 50_000_627, 50_001_274);
  return { title: "probe", timebase: NS, length: 60_000_000, channels: [{ kind: "digital", name: "CLK", end: 60_000_000, initial: 0, ticks: new Float64Array(ticks) }] };
}

/** A source that counts the edge ticks it hands out, to show a search reads little. */
function counting(source: Source): { source: Source; read: () => number } {
  let read = 0;
  return {
    source: new Proxy(source, {
      get(target, key) {
        const v = Reflect.get(target, key);
        if (key !== "edges" || typeof v !== "function") return typeof v === "function" ? v.bind(target) : v;
        return async (...args: Parameters<Source["edges"]>) => {
          const e = await target.edges(...args);
          read += e.ticks.length;
          return e;
        };
      },
    }),
    read: () => read,
  };
}

test("a pulse from the edges in hand: its level and edges, and what lies past them", async () => {
  const src = new MemorySource(recording());
  const e = await src.edges(0, 100, 300);
  // Tick 112 is in the low between the edges at 110 and 125; its period runs to the next fall, at 135.
  assert.deepEqual(pulseFromEdges(e, 100, 300, { start: 0, end: 60_000_000 }, 112), { level: 0, start: 110, end: 125, next: 135 });
  // An edge at the tick starts the pulse there.
  assert.deepEqual(pulseFromEdges(e, 100, 300, { start: 0, end: 60_000_000 }, 125), { level: 1, start: 125, end: 135, next: 150 });
  // Before the first edge, from the capture's start: no edge there to find.
  const head = await src.edges(0, 0, 105);
  assert.deepEqual(pulseFromEdges(head, 0, 105, { start: 0, end: 60_000_000 }, 50), { level: 0, start: null, end: 100, next: undefined });
  // Edges past the span read are not known yet.
  const mid = await src.edges(0, 10_000_000, 10_000_100);
  // The clock's 80 edges leave the quiet stretch low.
  assert.deepEqual(pulseFromEdges(mid, 10_000_000, 10_000_100, { start: 0, end: 60_000_000 }, 10_000_050), { level: 0, start: undefined, end: undefined, next: undefined });
});

test("a channel's data runs to one tick before its end, and no edge lies past a span that reaches it", async () => {
  const src = new MemorySource(recording());
  const clk = (await src.info()).channels[0];
  assert.equal(inChannel(clk, 59_999_999), true);
  assert.equal(inChannel(clk, 60_000_000), false);
  assert.equal(inChannel(clk, -1), false);
  // A span read up to the end holds every edge there is: the pulse runs to the end of what is known.
  const tail = await src.edges(0, 59_999_990, 60_000_000);
  assert.deepEqual(pulseFromEdges(tail, 59_999_990, 60_000_000, { start: 0, end: 60_000_000 }, 59_999_995), { level: 1, start: undefined, end: null, next: null });
  // One tick short of the end, an edge could still sit on the last tick.
  const short = await src.edges(0, 59_999_990, 59_999_999);
  assert.deepEqual(pulseFromEdges(short, 59_999_990, 59_999_999, { start: 0, end: 60_000_000 }, 59_999_995), { level: 1, start: undefined, end: undefined, next: undefined });
});

test("the edges of a long quiet stretch are found through the overview, reading few edges", async () => {
  const { source, read } = counting(new MemorySource(recording()));
  assert.equal(await findEdge(source, 0, 10_000_000, 60_000_001, 1, undefined), 50_000_000);
  assert.equal(await findEdge(source, 0, 1, 10_000_001, -1, undefined), 1_085);
  assert.equal(await findEdge(source, 0, 50_001_275, 60_000_001, 1, undefined), null);
  assert.ok(read() < 200, `read ${read()} edge ticks`);
  const p = await completePulse(source, 0, { level: 0, start: undefined, end: undefined, next: undefined }, 10_000_050, { start: 0, end: 60_000_000 });
  assert.deepEqual(p, { level: 0, start: 1_085, end: 50_000_000, next: 50_000_627 });
});

test("a pulse the source stamped with one time: the pulse before it ends there, and so does its period", async () => {
  // High from 10, a low pulse at 20 too short to time, high to 30; then quiet until a pair at 50,000,000.
  const ticks = new Float64Array([10, 20, 20, 30, 50_000_000, 50_000_000, 50_000_500]);
  const source = new MemorySource({ title: "pair", timebase: NS, length: 60_000_000, channels: [{ kind: "digital", name: "D", end: 60_000_000, initial: 0, ticks }] });
  const e = await source.edges(0, 0, 100);
  assert.deepEqual(pulseFromEdges(e, 0, 100, { start: 0, end: 60_000_000 }, 19), { level: 1, start: 10, end: 20, next: 20 });
  assert.deepEqual(pulseFromEdges(e, 0, 100, { start: 0, end: 60_000_000 }, 20), { level: 1, start: 20, end: 30, next: undefined });
  // That pulse's duty is probe.json's.
  // Found by search: the period ends at the pair's second edge, not at the edge after it.
  const p = await completePulse(source, 0, { level: 0, start: undefined, end: undefined, next: undefined }, 10_000_000, { start: 0, end: 60_000_000 });
  assert.deepEqual(p, { level: 0, start: 30, end: 50_000_000, next: 50_000_000 });
});

const NB = "\u00a0";

test("a pixel too crowded for a pulse reads its edges and its time high, exactly, from edges", () => {
  // High at 0; edges at 10, 20, 25 and 40, two at 50. The pixel [12, 42): low from the edge at
  // 10 to 20, high to 25, low to 40, high to 42: 7 of its 30 ticks high, and three edges.
  const e = { level: 1 as const, ticks: Float64Array.from([10, 20, 25, 40, 50, 50]) };
  const d = denseFromEdges(e, 12, 42, { start: 0, end: 100 });
  assert.deepEqual(d, { edges: [3, 3], ticks: 30, exact: { high: 7, known: 30 } });
  assert.deepEqual(denseFigures(d, NS), [`3 edges in 30${NB}ns`, `high 23.3${NB}%`]);
  // An edge at the pixel's first tick counts, and sets the level there; two at one tick count
  // twice and hold no time. [40, 52): high from 40, low and high again at 50: high throughout.
  assert.deepEqual(denseFromEdges(e, 40, 52, { start: 0, end: 100 }), { edges: [3, 3], ticks: 12, exact: { high: 12, known: 12 } });
  // Held to the channel's known span, high after the pair: past its end nothing is counted.
  assert.deepEqual(denseFromEdges(e, 90, 120, { start: 0, end: 100 }), { edges: [0, 0], ticks: 10, exact: { high: 10, known: 10 } });
});

test("from an overview, the bounds its buckets' densities and duties give, over the buckets the pixel meets, whole", () => {
  const o = (density: number[], duty: number[]) => ({ kind: "digital" as const, bucket: 100, start: 0, density: Uint8Array.from(density), duty: Uint8Array.from(duty) });
  const ov = o([0, 9, 5, 13, 1, 1, 0], [14, 10, 12, 7, 0, 14, 15]);
  // Bucket 1 alone: density 9 is four edges exactly; duty 10 is a share from 19/28 to 21/28.
  const one = denseFromOverview(ov, 150, 160, 10_000);
  assert.deepEqual(one, { edges: [4, 4], ticks: 100, share: [19 / 28, 21 / 28] });
  assert.deepEqual(denseFigures(one, NS), [`4 edges in 100${NB}ns`, `high 67.9 to 75.0${NB}%`]);
  // Buckets 1 and 2: four and two edges; the shares weighted by their ticks.
  const two = denseFromOverview(ov, 150, 250, 10_000);
  assert.deepEqual(two, { edges: [6, 6], ticks: 200, share: [(19 + 23) / 56, (21 + 25) / 56] });
  assert.deepEqual(denseFigures(two, NS), [`6 edges in 200${NB}ns`, `high 75.0 to 82.1${NB}%`]);
  // Density 13 stands for eight or nine edges.
  assert.deepEqual(denseFigures(denseFromOverview(ov, 300, 310, 10_000), NS)[0], `8 to 9 edges in 100${NB}ns`);
  // A bucket with an edge and duty 0 spent some time high, under 1/28 of it; duty 14, over 27/28.
  assert.deepEqual(denseFigures(denseFromOverview(ov, 400, 410, 10_000), NS), [`1 edge in 100${NB}ns`, `high under 3.6${NB}%`]);
  assert.deepEqual(denseFigures(denseFromOverview(ov, 500, 510, 10_000), NS), [`1 edge in 100${NB}ns`, `high over 96.4${NB}%`]);
  // A bucket outside the kept span adds no time; the last is held to the length.
  assert.deepEqual(denseFromOverview(ov, 550, 650, 640), { edges: [1, 1], ticks: 140, share: [27 / 28, 1] });
  // And weighs as what it holds of it: 100 ticks under 1/28 high, then 50 over 27/28.
  const cut = o([0, 1, 1], [14, 0, 14]);
  assert.deepEqual(denseFromOverview(cut, 150, 250, 250), { edges: [2, 2], ticks: 150, share: [(100 * 0 + 50 * (27 / 28)) / 150, (100 * (1 / 28) + 50 * 1) / 150] });
  assert.deepEqual(denseFigures(denseFromOverview(o([0], [15]), 0, 10, 100), NS), [`0 edges in 100${NB}ns`]);
});
