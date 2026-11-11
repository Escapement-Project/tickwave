// The committed fixture against what the Saleae export itself says: edge counts from the files'
// sizes ((size - 60) / 8), ends and first edges from an independent decode of the export.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { readFixture } from "../src/fixture-file.ts";
import { formatLevels, MemorySource } from "../src/memory-source.ts";
import { fromCore, indexAnnotations, laneAnnotations, ofKind, resolveRaw, type RawAnnotation } from "../src/annotations.ts";
import { sampleAtOrBefore, type AnalogChannel } from "../src/source.ts";

const file = readFileSync(new URL("../fixtures/identify-swd.tickwavefx", import.meta.url));

test("identify-swd: counts, ends and first edges match the export", async () => {
  const rec = await readFixture(file);
  assert.deepEqual(rec.timebase, { num: 1e9, den: 1 });
  assert.equal(rec.length, 523583100);
  const [clk, dio] = rec.channels;
  assert.ok(clk.kind === "digital" && dio.kind === "digital");
  assert.equal(clk.name, "SWCLK");
  assert.equal(clk.ticks.length, (122348 - 60) / 8);
  assert.equal(dio.ticks.length, (15052 - 60) / 8);
  assert.equal(clk.end, 523583070);
  assert.equal(dio.end, 523583100);
  assert.equal(clk.initial, 1);
  assert.equal(dio.initial, 1);
  assert.deepEqual([...clk.ticks.subarray(0, 3)], [376719436, 376720063, 376720710]);
  assert.deepEqual([...dio.ticks.subarray(0, 2)], [376845415, 376846682]);
});

test("identify-swd: edges around the first DPIDR read", async () => {
  const src = new MemorySource(await readFixture(file));
  const e = await src.edges(1, 377094000, 377107000);
  assert.equal(e.level, 0);
  assert.deepEqual([...e.ticks], [377094556, 377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074]);
});

test("a truncated fixture is refused", async () => {
  const { gunzipSync } = await import("node:zlib");
  const raw = gunzipSync(file);
  await assert.rejects(readFixture(raw.subarray(0, raw.length - 1)));
});

test("overview levels follow FORMAT.md", () => {
  // As the converter wrote them into the reconverted headers, the head's levels and the block
  // levels together: identify-swd [12, 16, 20] and none in blocks; gcc-swd-reads [16, 20] and 12
  // in blocks; stock-reads [16, 20, 24] and 12; the matrix capture [20, 24, 28] and 12 and 16.
  assert.deepEqual(formatLevels(523583100), [2 ** 12, 2 ** 16, 2 ** 20]);
  assert.deepEqual(formatLevels(927888190), [2 ** 12, 2 ** 16, 2 ** 20]);
  assert.deepEqual(formatLevels(4429986070), [2 ** 12, 2 ** 16, 2 ** 20, 2 ** 24]);
  assert.deepEqual(formatLevels(114006396250), [2 ** 12, 2 ** 16, 2 ** 20, 2 ** 24, 2 ** 28]);
  // 2^22 ticks fill exactly 1,024 buckets of 2^12, so no coarser level is needed; one tick more
  // takes a 1,025th.
  assert.deepEqual(formatLevels(2 ** 22), [2 ** 12]);
  assert.deepEqual(formatLevels(2 ** 22 + 1), [2 ** 12, 2 ** 16]);
});

test("annotations: generated ids, refs, times and errors", () => {
  const raw: RawAnnotation[] = [
    { id: "read/377094557", source: "swd", kind: "read", start: 377094557, end: 377140560, label: "DPIDR read", values: { asked: "1 MHz" } },
    { id: "dpidr-read", ref: "swd/read/377094557", label: "Reading the debug port's ID register" },
    { id: "attach", start: "376.719436 ms", end: "377.140560 ms", kind: "attach" },
    { id: "bad", ref: "swd/nothing" },
    { id: "a/b", start: 1 },
    { id: "attach", start: 1 },
  ];
  const r = resolveRaw(raw, { num: 1e9, den: 1 });
  const named = r.byId.get("dpidr-read")!;
  assert.equal(named.start, 377094557);
  assert.equal(named.kind, "read");
  assert.equal(named.ref, "swd/read/377094557");
  assert.deepEqual(named.values, { asked: "1 MHz" }, "an alias takes its target's values");
  assert.equal(r.byId.get("attach")!.values, null);
  assert.ok(r.named.has("swd/read/377094557"));
  assert.equal(r.byId.get("attach")!.start, 376719436);
  assert.equal(r.byId.get("attach")!.end, 377140560);
  assert.equal(r.errors.length, 3);
});

test("a resolved list: an alias names its target by ref; a matching span alone names nothing", () => {
  const core = [
    { id: "dpidr-read", source: null, start: 377094557, end: 377140560, channels: ["SWCLK", "SWDIO"], label: "Reading DPIDR", kind: "read", text: null, paint: null, measure: null, values: null, ref: "swd/read/377094557" },
    { id: "swd/read/377094557", source: "swd", start: 377094557, end: 377140560, channels: ["SWCLK", "SWDIO"], label: "DPIDR read", kind: "read", text: null, paint: null, measure: null, values: null },
    // Hand-written on exactly a generated annotation's span and channels, but no ref.
    { id: "same-span", source: null, start: 377094557, end: 377102558, channels: ["SWDIO"], label: "here", kind: null, text: null, paint: null, measure: null, values: null },
    { id: "swd/request/377094557", source: "swd", start: 377094557, end: 377102558, channels: ["SWDIO"], label: "request", kind: "request", text: null, paint: "probe", measure: null, values: null },
  ].map(fromCore);
  const r = indexAnnotations(core);
  assert.deepEqual([...r.named], ["swd/read/377094557"]);
  // The list keeps sidecar order; byTime is by start, then longer first.
  assert.deepEqual(r.list.map((a) => a.id), ["dpidr-read", "swd/read/377094557", "same-span", "swd/request/377094557"]);
  assert.equal(r.byTime[0].end, 377140560);
  assert.equal(fromCore({ ...core[2], channels: [] } as never).channels, null);
});

test("the lane and stepping take time order, whatever the sidecar's order", () => {
  // Sidecar order puts the hand-written annotations first, though they come later in time.
  const raw: RawAnnotation[] = [
    { id: "late-wait", start: 380392379, end: 380395379, kind: "wait", label: "a later WAIT", channels: ["SWDIO"] },
    { id: "wait/379065159", source: "swd", start: 379065159, end: 379068159, kind: "wait", label: "ack WAIT", channels: ["SWDIO"] },
    { id: "wait/381000000", source: "swd", start: 381000000, end: 381003000, kind: "wait", label: "ack WAIT", channels: ["SWDIO"] },
  ];
  const r = resolveRaw(raw, { num: 1e9, den: 1 });
  assert.deepEqual(r.list.map((a) => a.id), ["late-wait", "swd/wait/379065159", "swd/wait/381000000"]);
  assert.deepEqual(ofKind(r, "wait").map((a) => a.id), ["swd/wait/379065159", "late-wait", "swd/wait/381000000"]);
  assert.deepEqual(
    laneAnnotations(r, new Set(["SWCLK", "SWDIO"]), null).map((a) => a.id),
    ["swd/wait/379065159", "late-wait", "swd/wait/381000000"],
  );
});

test("a cursor reads the last sample at or before it", () => {
  // verge's energy capture: 100 kHz, sample i at tick 5000 + 10000 i, 306,178 samples.
  const c: AnalogChannel = { kind: "analog", name: "current", end: 3061780000, unit: "A", first: 5000, period: { num: 10000, den: 1 }, count: 306178, resolution: 1e-9, min: 0, max: 1 };
  assert.equal(sampleAtOrBefore(c, 4999), null);
  assert.equal(sampleAtOrBefore(c, 5000), 0);
  // Between samples 1 (15,000) and 2 (25,000): sample 1, not the nearer sample 2.
  assert.equal(sampleAtOrBefore(c, 24999), 1);
  assert.equal(sampleAtOrBefore(c, 25000), 2);
  assert.equal(sampleAtOrBefore(c, 3061779999), 306177);
  assert.equal(sampleAtOrBefore(c, 3061780000), null);
});

test("a fixture's measure of a kind the sidecar doesn't have is refused", () => {
  // As JSON gives it: `what` is only as typed as the file.
  const raw = JSON.parse('[{"id": "a", "start": 1, "end": 2, "measure": {"channel": "X", "what": "width"}}]') as RawAnnotation[];
  const r = resolveRaw(raw, { num: 1e9, den: 1 });
  assert.deepEqual(r.errors, ['annotation a measures "width", not rate, period or duration']);
  assert.equal(r.list.length, 0);
});
