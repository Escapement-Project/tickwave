// A merged recording whose second source's clock is stretched 1,000 ppm and whose tick 0 lies
// 1,000 ticks in: each of its samples' source ticks lies at origin + floor(k × 1001/1000) on the
// axis (FORMAT.md, "Times"), worked here with BigInt from the header's own numbers. The merge runs
// through the CLI, the reading through the WASM decoder.

import { after, test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openTickwave } from "../scripts/node-wasm.ts";
import { channelSampleTick, channelStart, readingStep, sampleTick, type AnalogChannel } from "../src/source.ts";
import { formatInterval } from "../src/readouts.ts";

const REPO = new URL("../..", import.meta.url).pathname;
const OUT = mkdtempSync(join(tmpdir(), "tickwave-mapped-"));
after(() => rmSync(OUT, { recursive: true, force: true }));
const MERGED = join(OUT, "merged.tickwave");
execFileSync(
  "cargo",
  ["run", "-q", "--release", "-p", "escapement-tickwave-cli", "--", "merge", "web/fixtures/identify-swd.tickwave", "web/fixtures/swd50-cut.tickwave", "-o", MERGED, "--stretch", "2=+1000ppm", "--align", "2=1000"],
  { cwd: REPO, stdio: ["ignore", "ignore", "pipe"] },
);

/** Source tick `k` on the axis, by the header's origin, offset and m, in BigInt. */
function byHand(origin: number, offset: number, m: [number, number], k: number): number {
  const x = (BigInt(k + offset) * BigInt(m[0])) / BigInt(m[1]);
  return origin + Number(x);
}

test("a stretched source's samples lie where its mapping puts them, and readings keep to its tick", async () => {
  const r = await openTickwave(MERGED);
  const h = r.header;
  // Both at 1 GHz: the axis ticks at 1 GHz, and source 1's tick lasts 1001/1000 of one.
  assert.deepEqual(r.info.timebase, { num: 1e9, den: 1 });
  assert.deepEqual(h.sources[1].stretch, [1001, 1000]);
  const c = r.info.channels.find((c) => c.kind === "analog" && c.source === 1 && c.sourceName === "analog_0") as AnalogChannel;
  assert.ok(c && c.kind === "analog", "swd50-cut's SWCLK");
  const origin = h.sources[1].origin;
  assert.deepEqual(c.map, { origin, offset: 0, m: { num: 1001, den: 1000 } });
  // The 1,000 ticks of alignment and the merge's own start put its tick 0 past 1,000.
  assert.ok(origin >= 1000, `origin ${origin}`);
  const tick = (i: number) => byHand(origin, 0, [1001, 1000], sampleTick(c.first, c.period, i));
  for (const i of [0, 1, 999, 1000, 1001, 25_000, c.count - 1]) assert.equal(channelSampleTick(c, i), tick(i), `sample ${i}`);
  // Not the identity: sample 1000 sits a tick later than its source tick says.
  assert.equal(tick(1000) - tick(0), 1001);
  assert.equal(channelStart(c), tick(0));
  // The samples a window holds are those whose axis ticks fall in it, as the decoder cuts them.
  const at = channelSampleTick(c, 20_000);
  for (const [s, e] of [
    [at, at + 50],
    [at - 1, at + 1],
    [at + 1, at + 1002],
  ]) {
    const got = await r.samples(r.info.channels.indexOf(c), s, e);
    const want: number[] = [];
    for (let i = 0; i < c.count; i++) if (tick(i) >= s && tick(i) < e) want.push(i);
    assert.deepEqual([got.first, got.values.length], [want[0] ?? got.first, want.length], `[${s}, ${e})`);
  }
  // A reading of it keeps to its source's tick, 1.001 ns: to 1 ns. One of no channel, to the
  // finer, identify-swd's 1 ns.
  assert.deepEqual(readingStep(r.info, [r.info.channels.indexOf(c)]), { num: 1001, den: 1000 });
  assert.equal(formatInterval(12_345, r.info.timebase, readingStep(r.info, [r.info.channels.indexOf(c)])), "12.345 µs");
  assert.deepEqual(readingStep(r.info), { num: 1, den: 1 });
});
