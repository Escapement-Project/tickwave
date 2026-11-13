// Times to ticks from testdata/view/time.json, which the Rust view core's tests read too; the
// attributes, which only the web reads, below.

import { test } from "node:test";
import assert from "node:assert/strict";
import { parseTime, toTicks } from "../src/time.ts";
import { parsePad, parseScale, parseWindow, parseChannels, parseSweep } from "../src/attributes.ts";
import { ratio, runTable } from "./tables.ts";

const GS = { num: 1_000_000_000, den: 1 };

runTable("time", {
  to_ticks: (a) => {
    const t = parseTime(a.text);
    assert.ok(t, `${a.text} parses`);
    return toTicks(t, ratio(a.timebase));
  },
  parse_time: (a) => parseTime(a.text),
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
