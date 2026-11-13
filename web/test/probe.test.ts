// The view's point measurements: a pulse around a tick, the edges past what is loaded, the figures
// with their digits, and an analog channel at a pixel. Expected values are worked by hand. The
// pure cases are in testdata/view/probe.json, which the Rust view core's tests read too, with the
// pulses below written out over the edges these tests read; the cases that read a MemorySource
// are here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { MemorySource, type MemoryRecording } from "../src/memory-source.ts";
import { analogFromOverview, analogFromSamples, completePulse, findEdge, formatDuty, pulseFigures, pulseFromEdges, type EdgeAt } from "../src/probe.ts";
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
