// The traces' line weight as a page gives it in `--tickwave-line-weight`.

import { test } from "node:test";
import assert from "node:assert/strict";
import { LINE_WEIGHT, lineWeight } from "../src/palette.ts";

test("a line weight is a number of CSS pixels with or without px; anything else, or none, is the default", () => {
  const cases: [string, number][] = [
    ["2", 2],
    ["2px", 2],
    [" 1.5px", 1.5],
    ["2.5 px ", 2.5],
    ["3PX", 3],
    [".75", 0.75],
    ["+4", 4],
    ["1e1px", 10],
    ["", LINE_WEIGHT],
    ["0", LINE_WEIGHT],
    ["0px", LINE_WEIGHT],
    ["-2px", LINE_WEIGHT],
    ["thick", LINE_WEIGHT],
    ["2em", LINE_WEIGHT],
    ["2 2", LINE_WEIGHT],
    ["Infinity", LINE_WEIGHT],
    ["1e400", LINE_WEIGHT],
    ["calc(1px + 1px)", LINE_WEIGHT],
  ];
  for (const [css, weight] of cases) assert.equal(lineWeight(css), weight, JSON.stringify(css));
  assert.equal(LINE_WEIGHT, 1.5);
});
