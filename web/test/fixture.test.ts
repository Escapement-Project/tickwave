// The committed fixture against what the Saleae export itself says: edge counts from the files'
// sizes ((size - 60) / 8), ends and first edges from an independent decode of the export.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { readFixture } from "../src/fixture-file.ts";
import { formatLevels, MemorySource } from "../src/memory-source.ts";
import { resolveAnnotations, type RawAnnotation } from "../src/annotations.ts";

const file = readFileSync(new URL("../fixtures/identify-swd.chronofx", import.meta.url));

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
  // As the converter wrote them into the converted files' headers: [12, 16, 20] for identify-swd,
  // [16, 20] for gcc-swd-reads, [16, 20, 24] for stock-reads.
  assert.deepEqual(formatLevels(523583100), [2 ** 12, 2 ** 16, 2 ** 20]);
  assert.deepEqual(formatLevels(927888190), [2 ** 16, 2 ** 20]);
  assert.deepEqual(formatLevels(4429986070), [2 ** 16, 2 ** 20, 2 ** 24]);
});

test("annotations: generated ids, refs, times and errors", () => {
  const raw: RawAnnotation[] = [
    { id: "read/377094557", source: "swd", kind: "read", start: 377094557, end: 377140560, label: "DPIDR read" },
    { id: "dpidr-read", ref: "swd/read/377094557", label: "Reading the debug port's ID register" },
    { id: "attach", start: "376.719436 ms", end: "377.140560 ms", kind: "attach" },
    { id: "bad", ref: "swd/nothing" },
    { id: "a/b", start: 1 },
    { id: "attach", start: 1 },
  ];
  const r = resolveAnnotations(raw, { num: 1e9, den: 1 });
  const named = r.byId.get("dpidr-read")!;
  assert.equal(named.start, 377094557);
  assert.equal(named.kind, "read");
  assert.equal(named.ref, "swd/read/377094557");
  assert.ok(r.named.has("swd/read/377094557"));
  assert.equal(r.byId.get("attach")!.start, 376719436);
  assert.equal(r.byId.get("attach")!.end, 377140560);
  assert.equal(r.errors.length, 3);
});
