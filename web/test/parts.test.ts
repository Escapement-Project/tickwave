// A bundled sidecar's annotations read a range at a time (FORMAT.md, "Annotations"): the bundle
// holds the hand-written file and each generator file's first line, and the parts hold the lines,
// each fetched only where its span meets the range asked for. identify-swd.bundle.tickwave holds
// two parts, ticks 376,719,436 to 387,020,485 and 387,012,484 to 389,759,129.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { MemoryBytes } from "../src/wasm/bytes.ts";
import { handJson } from "../src/wasm/hand.ts";
import { Opened } from "../src/wasm/opened.ts";
import type { CoreAnnotation } from "../src/annotations.ts";
import { wasm } from "../scripts/node-wasm.ts";
import { layout, partRows } from "./layout.ts";

const at = (p: string) => new URL(`../${p}`, import.meta.url);
const FILE = new Uint8Array(readFileSync(at("fixtures/identify-swd.bundle.tickwave")));

/** Whether any read of `bytes` took any byte of `[from, to)`. */
const readAny = (bytes: MemoryBytes, [from, to]: readonly [number, number]) => bytes.reads.some(([a, b]) => a < to && b > from);

test("a span reads the parts it meets and no other, each once; every part read, it is the sidecar resolved whole", async () => {
  const l = layout(FILE);
  const rows = partRows(FILE, l.parts!);
  assert.deepEqual(
    rows.map((r) => [r.start, r.end]),
    [
      [376_719_436, 387_020_485],
      [387_012_484, 389_759_129],
    ],
  );
  const bytes = new MemoryBytes(FILE, "identify-swd.bundle.tickwave");
  // A first read of the footer alone, so the parts come by range.
  const r = await Opened.open(wasm(), bytes, FILE.length - l.footer);
  assert.ok(!readAny(bytes, [rows[0].from, rows[1].to]));
  const first = (await r.annotations([0, 380_000_000]))!;
  assert.equal(first.complete, false);
  assert.ok(readAny(bytes, [rows[0].from, rows[0].to]));
  assert.ok(!readAny(bytes, [rows[1].from, rows[1].to]), "the part the span misses");
  // Its generated annotations all start in the part's span, and the hand-written file's refs wait
  // for every part, as do the bundle's problems.
  const generated = first.list.filter((a) => a.source !== null);
  assert.ok(generated.length > 0);
  assert.ok(generated.every((a) => a.start >= rows[0].start && a.start <= rows[0].end));
  assert.ok(first.list.every((a) => !a.ref));
  assert.deepEqual(first.problems, []);
  // The same span again reads nothing.
  const reads = bytes.reads.length;
  assert.deepEqual(await r.annotations([0, 380_000_000]), first);
  assert.equal(bytes.reads.length, reads);
  // A span the second part meets reads it; the first comes from what was read.
  const second = (await r.annotations([388_000_000, 389_000_000]))!;
  assert.ok(readAny(bytes, [rows[1].from, rows[1].to]));
  assert.equal(second.complete, true);
  assert.equal((await r.annotations(null))!.complete, true);
  // Every part read: the list is the sidecar's files resolved whole, by the resolver a sidecar
  // beside a recording goes through, refs and all.
  const { json } = handJson(readFileSync(at("fixtures/identify-swd.annotations.toml"), "utf8"), "identify-swd.annotations.toml");
  const whole = JSON.parse(r.resolveAnnotations(json, ["identify-swd.swd.jsonl"], [readFileSync(at("fixtures/identify-swd.swd.jsonl"), "utf8")], null)) as CoreAnnotation[];
  assert.ok(whole.some((a) => a.ref));
  assert.deepEqual(second.list, whole);
  assert.deepEqual(second.problems, []);
});

test("a recording that bundles none reads nothing for its annotations", async () => {
  const one = new MemoryBytes(new Uint8Array(readFileSync(at("fixtures/identify-swd.tickwave"))), "identify-swd.tickwave");
  const r = await Opened.open(wasm(), one);
  const reads = one.reads.length;
  assert.equal(await r.annotations(null), null);
  assert.equal(one.reads.length, reads);
});
