// Analog samples whose places fall between ticks (FORMAT.md, "Times"): a channel's declared phase,
// a break's own phase, and a period that isn't whole, each place mapped to the axis tick at or
// before it. fixtures/phased.tickwave is crates/format/examples/phased.rs's; its places are worked
// by hand below. The sweep holds the search to a bisection over the ticks, on the cases
// crates/view/src/runs.rs's test takes, and each tick to the format's rule worked in BigInt.

import { test } from "node:test";
import assert from "node:assert/strict";
import { openTickwave } from "../scripts/node-wasm.ts";
import {
  channelIndexAtOrAfter,
  channelSampleTick,
  indicesAtOrAfter,
  knownSpans,
  lostSpans,
  placeAfter,
  sampleAtOrBefore,
  sampleTicks,
  type AnalogChannel,
  type Break,
  type Mapping,
  type Ratio,
} from "../src/source.ts";

// A: places 100.75 + 2.5 i for i < 10, then 140.25 + 2.5 (i - 10); the tick at or before each.
const A_TICKS = [100, 103, 105, 108, 110, 113, 115, 118, 120, 123, 140, 142, 145, 147, 150, 152, 155, 157, 160, 162];
// B: places 100 + 2.5 i.
const B_TICKS = [100, 102, 105, 107, 110, 112, 115, 117, 120, 122, 125, 127, 130, 132, 135, 137, 140, 142, 145, 147];

test("a phase and a break's phase place a recording's samples at the ticks their places map to", async () => {
  const r = await openTickwave("fixtures/phased.tickwave");
  const [a, b] = r.info.channels;
  assert.ok(a.kind === "analog" && b.kind === "analog");
  assert.deepEqual([a.phase, a.breaks, b.phase], [{ num: 3, den: 4 }, [{ index: 10, tick: 140, lost: 2, phase: 1 }], undefined]);
  assert.deepEqual(A_TICKS.map((_, i) => channelSampleTick(a, i)), A_TICKS);
  assert.deepEqual(B_TICKS.map((_, i) => channelSampleTick(b, i)), B_TICKS);
  // The sample a cursor reads: A's second sample lies at 103, so at 102 the first holds; B's
  // second lies at 102. After the break, A's twelfth lies at 142, a quarter tick past it.
  assert.deepEqual([102, 103, 141, 142, 143].map((t) => sampleAtOrBefore(a, t)), [0, 1, 10, 11, 11]);
  assert.deepEqual([101, 102].map((t) => sampleAtOrBefore(b, t)), [0, 1]);
  assert.deepEqual([101, 103, 104, 141, 142].map((t) => channelIndexAtOrAfter(a, t)), [1, 1, 2, 11, 11]);
  // A's two lost samples sat at places 125.75 and 128.25 on the first run's grid, to 130.75; its
  // time ends after its last sample's place, 165.25.
  assert.deepEqual(lostSpans(a), [{ from: 125, to: 130, lost: 2 }]);
  assert.equal(placeAfter(a, 19), 165);
  assert.equal(a.end, 165);
  assert.deepEqual(knownSpans(a, 0, 400), [
    [100, 125],
    [140, 165],
  ]);
  // The decoder's values come back at those places: A's samples from tick 102 to 104 are its
  // second alone, 1.001 V.
  const s = await r.samples(0, 102, 104);
  assert.deepEqual([s.first, [...s.values]], [1, [1.001]]);
  const after = await r.samples(0, 141, 143);
  assert.deepEqual([after.first, [...after.values]], [11, [1.011]]);
  // Lost samples end at the next run's start where that comes sooner: four lost from place 5 on a
  // whole-tick grid, the run after them starting at 7.
  const cut: AnalogChannel = { kind: "analog", name: "x", end: 20, unit: "V", first: 0, period: { num: 1, den: 1 }, count: 10, breaks: [{ index: 5, tick: 7, lost: 4 }], resolution: null, min: 0, max: 1 };
  assert.deepEqual(lostSpans(cut), [{ from: 5, to: 7, lost: 4 }]);
});

/** The format's rule in BigInt: the axis tick of sample `i`'s place (FORMAT.md, "Times"). */
function reference(c: AnalogChannel, i: number): number {
  const period = c.period;
  const phase = c.phase ?? { num: 0, den: 1 };
  const g = (a: bigint, b: bigint): bigint => (b === 0n ? a : g(b, a % b));
  const pd = BigInt(period.den) / g(BigInt(period.num), BigInt(period.den));
  const fd = BigInt(phase.den) / g(BigInt(phase.num), BigInt(phase.den) || 1n);
  const d = (pd / g(pd, fd)) * fd;
  const breaks = c.breaks ?? [];
  let run = { index: 0, tick: c.first, phase: (BigInt(phase.num) * d) / BigInt(phase.den) };
  for (const b of breaks) if (b.index <= i) run = { index: b.index, tick: b.tick, phase: BigInt(b.phase ?? 0) };
  // Place in 1/d ticks: tick d + phase + (i - index) period d.
  const units = BigInt(run.tick) * d + run.phase + (BigInt(i - run.index) * BigInt(period.num) * d) / BigInt(period.den);
  const map = c.map ?? { origin: 0, offset: 0, m: { num: 1, den: 1 } };
  const n = (units + BigInt(map.offset) * d) * BigInt(map.m.num);
  const q = BigInt(map.m.den) * d;
  const t = n / q;
  return map.origin + Number(n % q !== 0n && n < 0n ? t - 1n : t);
}

const r = (num: number, den: number): Ratio => ({ num, den });
const brk = (index: number, tick: number, phase = 0): Break => ({ index, tick, lost: 0, ...(phase && { phase }) });
const map = (origin: number, offset: number, m: Ratio): Mapping => ({ origin, offset, m });
const far = 2 ** 50;
const channel = (first: number, count: number, period: Ratio, phase: Ratio | undefined, breaks: Break[], m?: Mapping): AnalogChannel => ({
  kind: "analog",
  name: "x",
  end: Infinity,
  unit: "V",
  first,
  period,
  ...(phase && { phase }),
  count,
  breaks,
  ...(m && { map: m }),
  resolution: null,
  min: 0,
  max: 1,
});
// crates/view/src/runs.rs's cases, the far one at 2^50 rather than 2^60: a JavaScript tick is a
// double, exact below 2^53.
const cases = [
  channel(5, 100, r(1, 1), undefined, []),
  channel(5, 100, r(960, 1), undefined, [brk(40, 50_000)], map(-7, 3, r(1, 1))),
  channel(5, 100, r(7, 3), r(1, 3), [brk(30, 200, 2), brk(31, 205)]),
  channel(5, 100, r(3, 2), r(1, 4), [], map(100, -12, r(1001, 1000))),
  channel(5, 100, r(1, 1), undefined, [brk(50, 70)], map(0, 0, r(999, 1000))),
  channel(5, 100, r(5, 4), undefined, [], map(10, 0, r(3, 1))),
  channel(5, 100, r(1, 1), undefined, [], map(0, 0, r(1, 2))),
  channel(far, 1000, r(1, 1), undefined, [brk(600, far + 700)]),
];

test("the search for a tick's sample finds what a bisection over the format's ticks finds", () => {
  let checked = 0;
  for (const c of cases) {
    const ticks = Array.from({ length: c.count }, (_, i) => reference(c, i));
    assert.deepEqual(
      ticks.map((_, i) => channelSampleTick(c, i)),
      ticks,
      JSON.stringify(c),
    );
    const bisect = (past: (t: number) => boolean) => {
      const i = ticks.findIndex(past);
      return i < 0 ? c.count : i;
    };
    const end = ticks[c.count - 1] + 3;
    const at = { ...c, end };
    for (let t2 = 2 * (ticks[0] - 3); t2 < 2 * end; t2++) {
      const tick = t2 / 2;
      assert.equal(channelIndexAtOrAfter(c, tick), bisect((t) => t >= tick), `${JSON.stringify(c)} at ${tick}`);
      const before = tick >= ticks[0] && tick < end ? bisect((t) => t > tick) - 1 : null;
      assert.equal(sampleAtOrBefore(at, tick), before === null || before < 0 ? null : before, `${JSON.stringify(c)} at ${tick}`);
      checked++;
    }
  }
  assert.ok(checked > 3000, `${checked}`);
});

test("held over many samples and many columns, placing and searching give what they give one at a time", () => {
  // The sweep's cases, and one whose places pass 2^53 in its grid's units (2^50 ticks in thirds),
  // where they are held in BigInt.
  const all = [...cases, channel(far, 100, r(7, 3), r(1, 3), [brk(50, far + 200, 2)])];
  let [columns, moved, stayed, leapt] = [0, 0, 0, 0];
  for (const c of all) {
    // Every sample, in order and then from the far end back, each run's first included.
    const tickOf = sampleTicks(c);
    const order = Array.from({ length: c.count }, (_, i) => i);
    for (const i of [...order, ...order.reverse()]) assert.equal(tickOf(i), channelSampleTick(c, i), `${JSON.stringify(c)} sample ${i}`);
    // Columns from three ticks before the first sample to past the last, narrower than a sample,
    // about one, and many samples wide; each answer what a lookup alone gives.
    const [lo, hi] = [channelSampleTick(c, 0) - 3, channelSampleTick(c, c.count - 1) + 3];
    for (const per of [0.37, 1, 2.5, 7.3, 61]) {
      const next = indicesAtOrAfter(c);
      let before = -1;
      for (let t = lo; t < hi; t += per) {
        const got = next(t);
        assert.equal(got, channelIndexAtOrAfter(c, t), `${JSON.stringify(c)}, columns of ${per} at ${t}`);
        if (got === before) stayed++;
        else if (got - before > 1) leapt++;
        else moved++;
        before = got;
        columns++;
      }
      // A tick lower than the one before is looked up afresh.
      assert.equal(next(lo + (hi - lo) / 3), channelIndexAtOrAfter(c, lo + (hi - lo) / 3));
      assert.equal(next(Number.NaN), c.count);
    }
  }
  // Not vacuous: answers that stay, step one sample on, and leap several.
  assert.ok(stayed > 1000 && moved > 1000 && leapt > 100, `${stayed} ${moved} ${leapt} of ${columns}`);
});
