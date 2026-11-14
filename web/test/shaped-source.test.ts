// The convert flow's preview of a move: each channel's data at its tick plus its offset, read from the
// recording as converted.

import { test } from "node:test";
import assert from "node:assert/strict";
import { MemorySource } from "../src/memory-source.ts";
import { ShapedSource } from "../src/shaped-source.ts";
import { channelStart, type RecordingInfo } from "../src/source.ts";

const rec = {
  title: "",
  timebase: { num: 1e9, den: 1 },
  length: 1_000,
  channels: [
    { kind: "digital" as const, name: "CLK", end: 1_000, initial: 0 as const, ticks: Float64Array.from([100, 200, 300, 400]) },
    { kind: "analog" as const, name: "V", end: 1_000, unit: "V", first: 0, period: { num: 10, den: 1 }, resolution: null, values: Float64Array.from({ length: 100 }, (_, i) => i) },
  ],
};

async function moved(offsets: number[]): Promise<{ s: ShapedSource; info: RecordingInfo }> {
  const inner = new MemorySource(rec, { levels: [16, 64] });
  const s = new ShapedSource(inner, await inner.info());
  offsets.forEach((d, i) => s.setOffset(i, d));
  return { s, info: await s.info() };
}

test("a channel moved later: its edges and its data's start move with it, and the length reaches its end", async () => {
  const { s, info } = await moved([50, 0]);
  assert.deepEqual(info.channels[0], { kind: "digital", name: "CLK", end: 1_050, start: 50 });
  assert.equal(info.length, 1_050);
  assert.deepEqual([...(await s.edges(0, 0, 400)).ticks], [150, 250, 350]);
  const e = await s.edges(0, 260, 1_050);
  // Moved, CLK rises at 150 and falls at 250: low at 260.
  assert.deepEqual([e.level, [...e.ticks]], [0, [350, 450]]);
  // The other channel stays where it was.
  assert.equal((await s.samples(1, 0, 30)).first, 0);
});

test("a channel moved earlier reaches before the capture's start, and its overview's buckets move with it", async () => {
  const { s, info } = await moved([0, -25]);
  const v = info.channels[1];
  // Its samples keep their own ticks; the source's tick 0 moves to -25.
  assert.equal(v.kind === "analog" && v.first, 0);
  assert.equal(channelStart(v), -25);
  assert.equal(v.end, 975);
  // Samples 0 and 1 sit at -25 and -15: a read from -30 to -10 takes both, by index.
  const got = await s.samples(1, -30, -10);
  assert.deepEqual([got.first, [...got.values]], [0, [0, 1]]);
  const ov = await s.overview(1, -25, 975, 64);
  assert.equal(ov?.start, -25);
  assert.throws(() => s.setOffset(0, 1.5), /whole number of ticks/);
});
