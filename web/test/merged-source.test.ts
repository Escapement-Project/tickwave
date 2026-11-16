// The merge's preview: several recordings on one axis at the least common multiple of their tick
// rates, or the fastest where that has a term past 2^53, each input's ticks mapped by its ratio
// plus its alignment. Expected values are worked by hand.

import { test } from "node:test";
import assert from "node:assert/strict";
import { MemorySource } from "../src/memory-source.ts";
import { mergeAxis, MergedSource, mergedTimebase } from "../src/merged-source.ts";
import { channelSampleTick, channelStart, readingStep, type RecordingInfo, type SourceHeader } from "../src/source.ts";
import { formatInterval } from "../src/readouts.ts";

/** An analyser at 500 MHz: CLK rises at 100 and falls at 200, its ticks. */
const analyser = {
  title: "analyser",
  timebase: { num: 500_000_000, den: 1 },
  length: 1_000,
  channels: [{ kind: "digital" as const, name: "CLK", end: 1_000, initial: 0 as const, ticks: Float64Array.from([100, 200]) }],
};

/** A probe at 96 MHz: its own CLK, and a current sampled every 3/2 of its ticks. */
const probe = {
  title: "probe",
  timebase: { num: 96_000_000, den: 1 },
  length: 300,
  channels: [
    { kind: "digital" as const, name: "CLK", end: 300, initial: 1 as const, ticks: Float64Array.from([10, 20, 30]) },
    { kind: "analog" as const, name: "I", end: 300, unit: "A", first: 0, period: { num: 3, den: 2 }, resolution: null, values: Float64Array.from({ length: 200 }, (_, i) => i) },
  ],
};

async function input(rec: typeof analyser | typeof probe, stem: string, sources?: SourceHeader[]) {
  const source = new MemorySource(rec, { levels: [16, 64] });
  const info: RecordingInfo = { ...(await source.info()), ...(sources && { sources }) };
  return { source, info, stem };
}

test("the axis ticks at the least common multiple of the rates", () => {
  // 500 MHz = 2^8 · 5^9, 96 MHz = 2^11 · 3 · 5^6: the multiple is 2^11 · 3 · 5^9 = 12 GHz.
  assert.deepEqual(mergedTimebase([{ num: 500e6, den: 1 }, { num: 96e6, den: 1 }]), { num: 12e9, den: 1 });
  // Rates with denominators: 3/2 and 5/4 ticks a second have 15/2 as their least common multiple.
  assert.deepEqual(mergedTimebase([{ num: 3, den: 2 }, { num: 5, den: 4 }]), { num: 15, den: 2 });
  // Unreduced terms reduce first: 6/4 is 3/2.
  assert.deepEqual(mergedTimebase([{ num: 6, den: 4 }]), { num: 3, den: 2 });
  assert.throws(() => mergedTimebase([{ num: 2 ** 52 + 1, den: 1 }, { num: 2 ** 52 - 1, den: 1 }]), /no common rate/);
});

test("each input's ticks scale by its ratio, and a later input's clashing name takes its stem", async () => {
  const s = new MergedSource([await input(analyser, "analyser"), await input(probe, "probe")]);
  // 12 GHz over 500 MHz is 24; over 96 MHz, 125.
  assert.deepEqual(s.ratios, [
    { num: 24, den: 1 },
    { num: 125, den: 1 },
  ]);
  const info = s.mergedInfo;
  assert.deepEqual(info.timebase, { num: 12e9, den: 1 });
  assert.deepEqual(
    info.channels.map((c) => [c.name, c.source]),
    [
      ["CLK", 0],
      ["probe/CLK", 1],
      ["I", 1],
    ],
  );
  // The analyser's 1,000 ticks are 24,000; the probe's 300 are 37,500, the longer.
  assert.equal(info.length, 37_500);
  assert.deepEqual([...(await s.edges(0, 0, 24_000)).ticks], [2_400, 4_800]);
  assert.deepEqual([...(await s.edges(1, 0, 37_500)).ticks], [1_250, 2_500, 3_750]);
  // The current keeps its period of 3/2 of its own ticks, each 125 merged ones: sample 1's place,
  // source tick 1.5, lies at merged tick 187.5, so 187 (FORMAT.md, "Times": the place maps, not
  // the whole tick before it).
  const i = info.channels[2];
  assert.ok(i.kind === "analog");
  assert.deepEqual([i.period, i.map?.m], [
    { num: 3, den: 2 },
    { num: 125, den: 1 },
  ]);
  assert.deepEqual([1, 2, 199].map((k) => channelSampleTick(i, k)), [187, 375, 37_312]);
});

test("the level at a span's start counts the edges of the input tick it starts inside", async () => {
  const s = new MergedSource([await input(analyser, "analyser"), await input(probe, "probe")]);
  const info = s.mergedInfo;
  assert.deepEqual(
    info.channels.map((c) => [channelStart(c), c.end]),
    [
      [0, 24_000],
      [0, 37_500],
      [0, 37_500],
    ],
  );
  // The probe's CLK starts high, falls at 1,250 and rises at 2,500: low just before 2,500.
  const e = await s.edges(1, 2_500, 5_000);
  assert.deepEqual([e.level, [...e.ticks]], [0, [2_500, 3_750]]);
  // From 2,501, inside the input tick whose rise lands at 2,500: the rise is behind, so high.
  const after = await s.edges(1, 2_501, 5_000);
  assert.deepEqual([after.level, [...after.ticks]], [1, [3_750]]);
  // Its overview's buckets: 16 input ticks are 2,000 merged ones.
  const ov = await s.overview(1, 0, 37_500, 2_000);
  assert.deepEqual([ov?.bucket, ov?.start], [2_000, 0]);
});

test("sources join the list, restated in merged ticks", async () => {
  const header = (rate: number, origin = 0, align?: number): SourceHeader => ({ format: "vcd", files: [], timebase: [rate, 1], origin, ...(align !== undefined && { align }) });
  // The probe's input is itself merged at its own rate: its source sits 3 of its ticks in, moved 2.
  const s = new MergedSource([await input(analyser, "analyser", [header(500e6)]), await input(probe, "probe", [header(96e6, 3, 2)])]);
  assert.deepEqual(
    s.mergedInfo.sources?.map((x) => [x.timebase, x.origin, x.align]),
    [
      [[500e6, 1], 0, undefined],
      [[96e6, 1], 375, 250],
    ],
  );
});

test("a reading keeps to its sources' ticks, never the merged one", async () => {
  const header = (rate: number): SourceHeader => ({ format: "vcd", files: [], timebase: [rate, 1], origin: 0 });
  const s = new MergedSource([await input(analyser, "analyser", [header(500e6)]), await input(probe, "probe", [header(96e6)])]);
  const info = s.mergedInfo;
  // One channel: its source's m. Both sources: the coarser. None: the finer.
  const [a, p] = [
    { num: 24, den: 1 },
    { num: 125, den: 1 },
  ];
  assert.deepEqual([readingStep(info, [0]), readingStep(info, [1]), readingStep(info, [0, 2]), readingStep(info)], [a, p, p, a]);
  // 2,400 ticks at 12 GHz are 200 ns. The merged tick is 83 ps, but the analyser's is 2 ns.
  assert.equal(formatInterval(2_400, info.timebase), "200.00\u00a0ns");
  assert.equal(formatInterval(2_400, info.timebase, a), "200\u00a0ns");
  // A recording of one source reads at its own tick.
  assert.deepEqual(readingStep(analyser as unknown as RecordingInfo, [0]), { num: 1, den: 1 });
});

test("rates whose multiple is past 2^53 merge at the fastest, a slower input's tick a ratio of its", async () => {
  // 3^33 and 2^52 ticks a second: their multiple has a term past 2^53, so the axis ticks at 3^33,
  // and each of the 2^52 input's ticks is 3^33 / 2^52 of its, about 1.234.
  const fast = { title: "fast", timebase: { num: 3 ** 33, den: 1 }, length: 200, channels: [{ kind: "digital" as const, name: "F", end: 200, initial: 0 as const, ticks: Float64Array.from([5]) }] };
  const slow = { title: "slow", timebase: { num: 2 ** 52, den: 1 }, length: 200, channels: [{ kind: "digital" as const, name: "S", end: 200, initial: 0 as const, ticks: Float64Array.from([10, 100]) }] };
  const sources = [new MemorySource(fast), new MemorySource(slow)];
  const s = new MergedSource(await Promise.all(sources.map(async (source, k) => ({ source, info: await source.info(), stem: ["fast", "slow"][k] }))));
  assert.deepEqual(s.mergedInfo.timebase, { num: 3 ** 33, den: 1 });
  assert.deepEqual(s.ratios[1], { num: 3 ** 33, den: 2 ** 52 });
  // floor(10 × 1.2343...) and floor(100 × 1.2343...); its end, floor(200 × 1.2343...).
  assert.deepEqual([...(await s.edges(1, 0, 1_000)).ticks], [12, 123]);
  assert.equal(s.mergedInfo.channels[1].end, 246);
  // From merged tick 13 the edge at 12 is behind: the level is high.
  const e = await s.edges(1, 13, 1_000);
  assert.deepEqual([e.level, [...e.ticks]], [1, [123]]);
  // Its buckets are no whole number of merged ticks: none.
  assert.equal(await s.overview(1, 0, 246, 1_000), null);
  // The real multiple still wins where it fits.
  assert.deepEqual(mergeAxis([{ num: 500e6, den: 1 }, { num: 96e6, den: 1 }]), { num: 12e9, den: 1 });
});
