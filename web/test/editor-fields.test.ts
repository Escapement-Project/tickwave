// What the annotation editor writes into the hand-written file's fields: times that read back as
// the tick they came from, and ids a page can cite.

import { test } from "node:test";
import assert from "node:assert/strict";
import { idFrom, timeText } from "../../site/editor-fields.ts";
import { parseTime, toTicks } from "../src/time.ts";

test("a time is written at the recording's resolution and reads back as its tick", () => {
  assert.equal(timeText(317858820, { num: 1e9, den: 1 }), "317.858820 ms");
  assert.equal(timeText(0, { num: 1e9, den: 1 }), "0 ns");
  // A tick that is no whole power of ten long: three samples a microsecond, 50/3 MHz.
  for (const [tick, timebase] of [
    [7, { num: 3_000_000, den: 1 }],
    [123456789, { num: 50_000_000, den: 3 }],
    [999, { num: 781_250, den: 1 }],
  ] as const) {
    const text = timeText(tick, timebase);
    assert.equal(typeof text, "string");
    assert.equal(toTicks(parseTime(String(text))!, timebase), tick, `${tick} at ${timebase.num}/${timebase.den}`);
  }
});

test("an id comes from the label, in lower case with dashes, and unlike the ids taken", () => {
  assert.equal(idFrom("The 12-cycle period before read data", new Set()), "the-12-cycle-period-before-read-data");
  assert.equal(idFrom("Café: ACK/NAK", new Set()), "cafe-ack-nak");
  assert.equal(idFrom("misread", new Set(["misread", "misread-2"])), "misread-3");
  assert.equal(idFrom("  ", new Set()), "annotation");
  assert.ok(idFrom("x".repeat(60), new Set()).length <= 40);
  assert.doesNotMatch(idFrom("a / b c", new Set()), /[\s/]/);
});
