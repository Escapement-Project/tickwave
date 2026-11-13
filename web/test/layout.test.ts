// The annotation lane and the time axis, with text measured at 6 px a character.

import { test } from "node:test";
import assert from "node:assert/strict";
import { layoutNotes, elide } from "../src/render/notes.ts";
import { timeAxis } from "../src/render/axis.ts";
import { ratio, runTable } from "./tables.ts";

const measure = (s: string) => s.length * 6;

test("notes: touching brackets share a lane; labels fit the room between them", () => {
  // The DPIDR read's request [0, 80), ack [90, 120), data [120, 450) at 1 px a tick.
  const out = layoutNotes(
    [
      { id: "request", start: 0, end: 80, label: "request" },
      { id: "ack", start: 90, end: 120, label: "ack OK" },
      { id: "data", start: 120, end: 450, label: "data" },
    ],
    (t) => t,
    0,
    500,
    1,
    measure,
  );
  assert.deepEqual(out.map((n) => [n.id, n.lane, n.label]), [
    ["request", 0, "request"],
    // "ack OK" is 36 px; the room between request's label and data's bracket is 120 - 8 - 90 = 22.
    ["ack", 0, null],
    ["data", 0, "data"],
  ]);
});

test("notes: a long label is cut, not allowed to push a bracket out", () => {
  const out = layoutNotes(
    [
      { id: "a", start: 0, end: 60, label: "a long label for a short span" },
      { id: "b", start: 100, end: 200, label: "b" },
    ],
    (t) => t,
    0,
    500,
    1,
    measure,
  );
  assert.equal(out.length, 2);
  // Room for a: 0 to 100 - 8 = 92 px, 15 characters; 14 and an ellipsis fit.
  assert.equal(out[0].label, "a long label f…");
  assert.equal(out[1].label, "b");
});

test("notes: a free lane with the most room is taken", () => {
  const out = layoutNotes(
    [
      { id: "p0", start: 0, end: null, label: "debug step,0" },
      { id: "p1", start: 40, end: null, label: "debug step,1" },
      { id: "p2", start: 80, end: null, label: "debug step,2" },
    ],
    (t) => t,
    0,
    500,
    2,
    measure,
  );
  assert.deepEqual(out.map((n) => [n.id, n.lane]).sort(), [["p0", 0], ["p1", 1], ["p2", 0]]);
});

test("elide: at least six characters, or nothing", () => {
  // 60 px holds ten characters: nine and the ellipsis.
  assert.equal(elide("line reset, 51 clocks", 60, measure), "line rese…");
  // 40 px holds five and the ellipsis, one short of six.
  assert.equal(elide("line reset, 51 clocks", 40, measure), null);
  assert.equal(elide("ack", 18, measure), "ack");
});

// The time axis, from testdata/view/axis.json, which the Rust view core's tests read too.
runTable("axis", {
  time_axis: (a) => timeAxis(a.start, a.end, a.width, ratio(a.timebase), (s) => s.length * a.measure_per_char, a.gap),
});
