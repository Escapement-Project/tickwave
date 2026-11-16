// The searches over sorted ticks.

import { test } from "node:test";
import assert from "node:assert/strict";
import { gallop, lowerBound } from "../src/search.ts";

test("gallop finds what lowerBound does, from any start, near or far", () => {
  let s = 12345;
  const rand = () => (s = (s * 1103515245 + 12345) % 2 ** 31) / 2 ** 31;
  const found = { atStart: 0, near: 0, far: 0, atEnd: 0 };
  for (const n of [0, 1, 2, 3, 7, 64, 1000]) {
    const a = Float64Array.from({ length: n }, () => Math.floor(rand() * 50));
    a.sort();
    for (let lo = 0; lo <= n; lo++) {
      for (const x of [-1, 0, 1, 7, 25, 49, 50, 51, a[lo] ?? 0, a[Math.min(n - 1, lo + 1)] ?? 0, a[n - 1] ?? 0]) {
        const want = lowerBound(a, x, lo);
        assert.equal(gallop(a, x, lo), want, `n ${n}, lo ${lo}, x ${x}`);
        if (want === n) found.atEnd++;
        else if (want === lo) found.atStart++;
        else if (want - lo <= 8) found.near++;
        else found.far++;
      }
    }
  }
  // Not vacuous: answers at the start, a few steps past it, far past it, and at the end.
  for (const [where, count] of Object.entries(found)) assert.ok(count > 0, where);
});
