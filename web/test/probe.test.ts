// The view's point measurements: a pulse around a tick, the edges past what is loaded, the figures
// with their digits, and an analog channel at a pixel. Expected values are worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import { MemorySource, type MemoryRecording } from "../src/memory-source.ts";
import { analogFromOverview, analogFromSamples, completePulse, findEdge, formatDuty, pulseFigures, pulseFromEdges } from "../src/probe.ts";
import type { AnalogChannel, Source } from "../src/source.ts";

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
  assert.deepEqual(pulseFromEdges(e, 100, 300, 60_000_000, 112), { level: 0, start: 110, end: 125, next: 135 });
  // An edge at the tick starts the pulse there.
  assert.deepEqual(pulseFromEdges(e, 100, 300, 60_000_000, 125), { level: 1, start: 125, end: 135, next: 150 });
  // Before the first edge, from the capture's start: no edge there to find.
  const head = await src.edges(0, 0, 105);
  assert.deepEqual(pulseFromEdges(head, 0, 105, 60_000_000, 50), { level: 0, start: null, end: 100, next: undefined });
  // Edges past the span read are not known yet.
  const mid = await src.edges(0, 10_000_000, 10_000_100);
  // The clock's 80 edges leave the quiet stretch low.
  assert.deepEqual(pulseFromEdges(mid, 10_000_000, 10_000_100, 60_000_000, 10_000_050), { level: 0, start: undefined, end: undefined, next: undefined });
});

test("the edges of a long quiet stretch are found through the overview, reading few edges", async () => {
  const { source, read } = counting(new MemorySource(recording()));
  assert.equal(await findEdge(source, 0, 10_000_000, 60_000_001, 1, undefined), 50_000_000);
  assert.equal(await findEdge(source, 0, 1, 10_000_001, -1, undefined), 1_085);
  assert.equal(await findEdge(source, 0, 50_001_275, 60_000_001, 1, undefined), null);
  assert.ok(read() < 200, `read ${read()} edge ticks`);
  const p = await completePulse(source, 0, { level: 0, start: undefined, end: undefined, next: undefined }, 10_000_050, 60_000_000);
  assert.deepEqual(p, { level: 0, start: 1_085, end: 50_000_000, next: 50_000_627 });
});

test("a pulse's figures: whole-tick widths, and a rate and a duty to the digits the ticks carry", () => {
  // SWCLK's first pulses in identify-swd: low for 627 ns from 376,719,436, then high for 647.
  const f = pulseFigures({ level: 0, start: 376_719_436, end: 376_720_063, next: 376_720_710 }, NS, 523_583_070);
  // The rate's last digit is at its uncertainty, 785 kHz / 1274 ≈ 0.6 kHz: the hundreds of Hz. The
  // width's reciprocal, 1.5949 MHz, is uncertain by 1.595 MHz / 627 ≈ 2.5 kHz: the kHz.
  assert.deepEqual(f, { level: "low", width: "627\u00a0ns", inverseWidth: "1.595\u00a0MHz", period: "1.274\u00a0µs", frequency: "784.9\u00a0kHz", duty: "50.78\u00a0%" });
  const open = pulseFigures({ level: 1, start: null, end: 376_719_436, next: 376_720_063 }, NS, 523_583_070);
  assert.equal(open.width, "at least 376.719436\u00a0ms");
  assert.equal(open.inverseWidth, null);
  assert.equal(open.period, null);
  // The last pulse: both edges known, no period after it.
  const last = pulseFigures({ level: 1, start: 100, end: 110, next: null }, NS, 1_000);
  assert.deepEqual([last.inverseWidth, last.period], ["100\u00a0MHz", null]);
  assert.equal(pulseFigures({ level: 1, start: null, end: null, next: null }, NS, 1_000).width, "the whole capture, 1.000\u00a0µs");
  // 3 of 10 ticks high is 30 % give or take 10: the digit is the tens.
  assert.equal(formatDuty(3, 10), "30\u00a0%");
  assert.equal(formatDuty(1, 3), "30\u00a0%");
  assert.equal(formatDuty(500, 1_000), "50.0\u00a0%");
});

test("an analog channel at a pixel: one sample's value, or the range the pixel draws", () => {
  const c: AnalogChannel = { kind: "analog", name: "V", end: 100, unit: "V", first: 0, period: { num: 1, den: 1 }, count: 100, resolution: 0.001, min: 0, max: 1 };
  const s = { first: 0, values: Float64Array.from({ length: 100 }, (_, i) => i / 100) };
  // A pixel narrower than a sample reads the sample at or before the tick.
  assert.deepEqual(analogFromSamples(c, s, 10.2, 10.7, 10.5), { kind: "value", value: 0.1 });
  // A pixel over several samples reads their range.
  assert.deepEqual(analogFromSamples(c, s, 10, 14, 12), { kind: "range", min: 0.1, max: 0.13 });
  const o = { kind: "analog" as const, bucket: 4, start: 0, min: new Float64Array([0, 0.5, Infinity]), max: new Float64Array([0.2, 0.9, -Infinity]) };
  assert.deepEqual(analogFromOverview(o, 2, 6), { kind: "range", min: 0, max: 0.9 });
  assert.deepEqual(analogFromOverview(o, 8, 12), { kind: "none" });
});
