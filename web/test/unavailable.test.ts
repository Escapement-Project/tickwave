// Channels this version can't read (FORMAT.md, "Unavailable channels"), from
// fixtures/unavailable.tickwave, which crates/format/examples/unavailable.rs writes: V requires a
// feature and E states a codec, neither known; P is derived from V, and Q requires the feature
// itself; S = I − J reads.

import { test } from "node:test";
import assert from "node:assert/strict";
import { fromCore, readable } from "../src/annotations.ts";
import { openTickwave } from "../scripts/node-wasm.ts";

test("channels this version can't read keep their places and names, say why, and refuse a read", async () => {
  const r = await openTickwave("fixtures/unavailable.tickwave");
  const channels = r.info.channels;
  assert.deepEqual(
    channels.map((c) => [c.name, c.kind]),
    [
      ["V", "unavailable"],
      ["I", "analog"],
      ["J", "analog"],
      ["D", "digital"],
      ["E", "unavailable"],
      ["P", "unavailable"],
      ["Q", "unavailable"],
      ["S", "analog"],
    ],
  );
  for (const c of [0, 4, 5, 6]) {
    const ch = channels[c];
    assert.ok(ch.kind === "unavailable" && ch.why.startsWith(`channel ${ch.name} is unavailable to this reader`), `${c}: ${JSON.stringify(ch)}`);
    assert.equal(ch.end, r.info.length);
  }
  // Nothing of one is read: the decoder is never asked.
  await assert.rejects(r.samples(0, 0, 1000), /^RangeError: channel V is unavailable/);
  await assert.rejects(r.edges(4, 0, 1000), /^RangeError: channel E is unavailable/);
  await assert.rejects(r.overview(5, 0, 1000, Infinity), /^RangeError: channel P is unavailable/);
  // The channels around them read as ever, a derived one too: sample i of I is 1000 + 37 i mod
  // 200 mV and of J 900 + 13 i mod 100, so S is their difference.
  const s = await r.samples(7, 0, 1000);
  const expected = Array.from({ length: 10 }, (_, i) => (1000 + ((i * 37) % 200) - (900 + ((i * 13) % 100))) / 1000);
  assert.deepEqual([...s.values], expected);
  const s7 = channels[7];
  assert.ok(s7.kind === "analog");
  assert.deepEqual([s7.derived, s7.resolution], [{ op: "subtract", operands: ["I", "J"] }, 1 / 1000]);
  assert.deepEqual([...(await r.edges(3, 0, 3500)).ticks], [1000, 2000, 3000]);
});

test("an annotation that names a channel this version can't read is left out, and says why; the rest stay", async () => {
  const r = await openTickwave("fixtures/unavailable.tickwave");
  const read = (await r.annotations(null))!;
  // The first z starts past the length: left out as its part is read, it claims no id, and the
  // second z, inside the length, stays.
  assert.deepEqual(read.problems, ["verge/z: starts at or past the length, 100000"]);
  assert.deepEqual(
    read.list.map((a) => [a.id, a.start]),
    [
      ["verge/a", 1000],
      ["verge/b", 2000],
      ["verge/c", 3000],
      ["verge/d", 4000],
      ["verge/e", 5000],
      ["verge/z", 6000],
    ],
  );
  const errors: string[] = [];
  const kept = readable(read.list.map(fromCore), r.info.channels, errors);
  // a is on I and V, b measures E's rate: both left out, though a names I too.
  assert.deepEqual(
    kept.map((a) => a.id),
    ["verge/c", "verge/d", "verge/e", "verge/z"],
  );
  assert.deepEqual(errors, ['verge/a: names channel "V", which this version can\'t read', 'verge/b: names channel "E", which this version can\'t read']);
});
