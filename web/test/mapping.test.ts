// The mapping's floor, `floor(x × a / b)`, against BigInt: random x across the safe integers and
// near ±2^53, at m values whose terms' product is small, near 2^53, and past it, so x × a passes
// 2^53 and the plain-number path does the work.

import { test } from "node:test";
import assert from "node:assert/strict";
import { axisTick, firstAt, mulDivFloor } from "../src/source.ts";

/** floor(x × a / b) in BigInt, b > 0. */
function reference(x: number, a: number, b: number): number {
  const n = BigInt(x) * BigInt(a);
  const d = BigInt(b);
  const q = n / d;
  return Number(n % d !== 0n && n < 0n ? q - 1n : q);
}

/** A deterministic generator of 53-bit integers, and of ones small enough that `x × a / b` stays
 * below 2^53 while `x × a` passes it. */
function* xs(seed: number, n: number, a: number, b: number): Generator<number> {
  let s = BigInt(seed);
  const MAX = BigInt(Number.MAX_SAFE_INTEGER);
  for (let i = 0; i < n; i++) {
    s = (s * 6364136223846793005n + 1442695040888963407n) & ((1n << 64n) - 1n);
    const v = Number(s % (2n * MAX + 1n) - MAX);
    yield v;
    // Near the ends too.
    yield Number(MAX - (s % 1000n));
    yield -Number(MAX - (s % 1000n));
    yield Math.trunc((v * b) / (a + b));
  }
}

const MS: [number, number][] = [
  [500617, 4000], // 96 MHz stretched 1,234 ppm on a 12 GHz axis
  [125, 24],
  [1001, 8],
  [3, 2],
  [94906263, 94906265], // a × b just below 2^53
  [94906267, 94906265], // a × b just past 2^53: the BigInt path
  [2 ** 26 + 1, 2 ** 26 - 1],
  [1, 2 ** 52 - 1],
];

test("the floor of x × a / b matches BigInt's at every m, in plain numbers wherever its terms allow", () => {
  let checked = 0;
  let viaParts = 0;
  for (const [a, b] of MS) {
    for (const x of xs(a ^ b, 3000, a, b)) {
      const want = reference(x, a, b);
      if (!Number.isSafeInteger(want)) continue;
      assert.equal(mulDivFloor(x, a, b), want, `floor(${x} × ${a} / ${b})`);
      checked++;
      if (!Number.isSafeInteger(x * a) && a * b <= Number.MAX_SAFE_INTEGER) viaParts++;
    }
  }
  assert.ok(checked > 20_000, `${checked} checked`);
  assert.ok(viaParts > 10_000, `${viaParts} took the plain-number path past 2^53`);
  // Worked in Python's integers: 2^44 × 500617 / 4000 is 2^39 × 500617 / 125, 2,201,736,850,249,351
  // and 672/4000 over; and -(2^42 + 1) × 500617 / 4000 floors down to -550,434,212,562,463.
  assert.equal(mulDivFloor(2 ** 44, 500617, 4000), 2_201_736_850_249_351);
  assert.equal(mulDivFloor(-(2 ** 42) - 1, 500617, 4000), -550_434_212_562_463);
});

test("a source tick's axis tick and the first source tick at an axis tick undo each other", () => {
  const map = { origin: -7, offset: 3, m: { num: 500617, den: 4000 } };
  for (const t of [0, 1, 125, 2 ** 40, 2 ** 44 + 17, 2 ** 50 - 5, -(2 ** 44)]) {
    const k = firstAt(map, t);
    assert.ok(axisTick(map, k) >= t && axisTick(map, k - 1) < t, `${t}: ${k}`);
  }
});
