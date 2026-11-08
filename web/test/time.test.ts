import { test } from "node:test";
import assert from "node:assert/strict";
import { parseTime, toTicks } from "../src/time.ts";
import { parsePad, parseScale, parseWindow, parseChannels, parseSweep } from "../src/attributes.ts";

const GS = { num: 1_000_000_000, den: 1 };
const ticks = (s: string, tb = GS) => {
  const t = parseTime(s);
  assert.ok(t, `${s} parses`);
  return toTicks(t, tb);
};

test("times to ticks, exactly", () => {
  // PLAN.md §3's example.
  assert.equal(ticks("317.85882 ms"), 317_858_820);
  assert.equal(ticks("1234"), 1234);
  assert.equal(ticks("100 us", { num: 3_000_000, den: 1 }), 300);
  assert.equal(ticks("2 µs"), 2000);
  // 124.5 ticks rounds half away from zero to 125. In floating point 0.0001245 * 1e6 is
  // 124.49999999999999, which rounds to 124.
  assert.equal(ticks("0.0001245 ms"), 125);
  // Half away from zero below zero too; Math.round(-2.5) is -2.
  assert.equal(ticks("-2.5 ns"), -3);
  assert.equal(parseTime("12 parsecs"), null);
  assert.equal(parseTime("ms"), null);
});

test("attributes", () => {
  assert.deepEqual(parseChannels("  SWCLK   SWDIO "), ["SWCLK", "SWDIO"]);
  assert.equal(parseChannels(""), null);
  const w = parseWindow("317.8 ms 318.0 ms");
  assert.equal(w?.kind, "span");
  if (w?.kind === "span") {
    assert.equal(toTicks(w.start, GS), 317_800_000);
    assert.equal(toTicks(w.end, GS), 318_000_000);
  }
  assert.deepEqual(parseWindow("dpidr-read"), { kind: "annotation", id: "dpidr-read" });
  assert.equal(parseWindow("a b"), null);
  assert.deepEqual(parsePad("10%"), { fraction: 0.1 });
  const pad = parsePad("50 ns");
  assert.ok("time" in pad && toTicks(pad.time, GS) === 50);
  const scale = parseScale("2 ns/px");
  assert.ok(scale);
  assert.equal(toTicks({ seconds: scale }, GS), 2);
  assert.equal(parseScale("2 ns"), null);
  assert.equal(parseSweep("off"), false);
  assert.equal(parseSweep(null), true);
});
