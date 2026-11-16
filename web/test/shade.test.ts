// How a digital column with edges reads. The fixed cases are in testdata/view/shade.json, which
// the Rust view core's tests read too.

import { band, count, countOf, counts, dutyShare, lookAt, looks, rails, rate, stroke } from "../src/render/shade.ts";
import { BUSY, FALL, HIGH, LOW, NONE, RISE } from "../src/render/columns.ts";
import { test } from "node:test";
import assert from "node:assert/strict";
import { ratio, runTable } from "./tables.ts";

const STATES: Record<string, number> = { NONE, LOW, HIGH, BUSY, RISE, FALL };

runTable("shade", {
  counts: (a) => counts(a.density)?.map(Number) ?? null,
  count: (a) => count(a.density),
  rate: (a) => rate(a.edges, a.ticks, ratio(a.timebase)),
  band: (a) => band(a.rate),
  stroke: (a) => stroke(a.band, a.spacing, a.width),
  rails: (a) => rails(a.high, a.edges),
  duty_share: (a) => dutyShare(a.duty),
  looks: (a) => {
    const c = a.columns;
    const cols = {
      state: Uint8Array.from(c.state, (s: string) => STATES[s]),
      at: Float64Array.from(c.at),
      edges: Float64Array.from(c.edges),
      high: Float64Array.from(c.high),
      known: Float64Array.from(c.known),
    };
    const l = looks(cols, a.shown[0], a.shown[1], a.width, a.per, ratio(a.timebase));
    return Array.from(l.kind, (_, k) => lookAt(l, k));
  },
});

test("countOf: the table holds every density's count", () => {
  for (let d = 0; d < 256; d++) assert.ok(Object.is(countOf(d), count(d)), `density ${d}`);
});
