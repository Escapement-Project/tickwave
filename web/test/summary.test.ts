// A closed view's facts, which follow the channels it shows, and the span an open one covers.

import { test } from "node:test";
import assert from "node:assert/strict";
import { shownExtent, summaryFacts } from "../src/summary.ts";
import type { RecordingInfo } from "../src/source.ts";

const digital = (name: string, end: number) => ({ kind: "digital" as const, name, end });

/** A cJTAG capture of 1 ms whose SWO line stops being known at 600 µs. */
const CJTAG: RecordingInfo = {
  title: "cjtag.tickwave",
  timebase: { num: 1_000_000_000, den: 1 },
  length: 1_000_000,
  channels: [digital("TCKC", 1_000_000), digital("TMSC", 1_000_000), digital("SWO", 600_000)],
  bytes: 4800,
};

const nb = (s: string) => s.replaceAll(" ", " ");

test("a view of every channel gives the recording's length and names them all", () => {
  const all = [["fact-length", nb("1.00 ms")], ["fact-channels", "3 channels: TCKC, TMSC, SWO"], ["fact-size", nb("4.7 KiB")]];
  assert.deepEqual(summaryFacts(CJTAG), all);
  // Nothing found to show names them all too, as the view then shows every row it can.
  assert.deepEqual(summaryFacts(CJTAG, []), all);
  // All of them in another order: the view's order.
  assert.deepEqual(summaryFacts(CJTAG, [2, 0, 1])[1], ["fact-channels", "3 channels: SWO, TCKC, TMSC"]);
});

test("a view of some channels names them out of the recording's, and gives the length they span", () => {
  assert.deepEqual(summaryFacts(CJTAG, [0, 1]), [["fact-length", nb("1.00 ms")], ["fact-channels", "2 of 3 channels: TCKC, TMSC"], ["fact-size", nb("4.7 KiB")]]);
  assert.deepEqual(summaryFacts(CJTAG, [1, 0])[1], ["fact-channels", "2 of 3 channels: TMSC, TCKC"]);
  // A channel named twice is shown once.
  assert.deepEqual(summaryFacts(CJTAG, [0, 0])[1], ["fact-channels", "1 of 3 channels: TCKC"]);
  // SWO alone ends at 600 µs; the size is still the file's.
  assert.deepEqual(summaryFacts(CJTAG, [2]), [["fact-length", nb("600 µs")], ["fact-channels", "1 of 3 channels: SWO"], ["fact-size", nb("4.7 KiB")]]);
});

test("a one-channel recording says one channel; one without a size leaves it out", () => {
  const one: RecordingInfo = { ...CJTAG, channels: [digital("TCKC", 1_000_000)], bytes: undefined };
  assert.deepEqual(summaryFacts(one), [["fact-length", nb("1.00 ms")], ["fact-channels", "1 channel: TCKC"]]);
});

test("the span a view covers: the length with every channel, else the furthest end, else the length", () => {
  assert.equal(shownExtent(CJTAG, [0, 1, 2]), 1_000_000);
  assert.equal(shownExtent(CJTAG, [2, 1, 0]), 1_000_000);
  assert.equal(shownExtent(CJTAG, [2]), 600_000);
  assert.equal(shownExtent(CJTAG, [2, 0]), 1_000_000);
  const never: RecordingInfo = { ...CJTAG, channels: [...CJTAG.channels, digital("NC", 0)] };
  assert.equal(shownExtent(never, [3]), 1_000_000);
});
