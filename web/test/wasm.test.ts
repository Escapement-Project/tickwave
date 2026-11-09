// The WASM connector in Node: the same reader and recording code the worker runs, over the real
// decoder. Expected values come from the Saleae export (file sizes, an independent decode), from
// the converter's own report, or from the .tickwavefx fixture, which my extractor wrote from the
// export without the Rust core: two pipelines that must agree.

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { HttpBytes, MemoryBytes, type Bytes, type Fetch } from "../src/wasm/bytes.ts";
import { Opened, setCacheBytes } from "../src/wasm/opened.ts";
import { handJson } from "../src/wasm/hand.ts";
import { FIRST_READ } from "../src/wasm/protocol.ts";
import { readFixture } from "../src/fixture-file.ts";
import { formatLevels, MemorySource } from "../src/memory-source.ts";
import { isAbort } from "../src/source.ts";
import { formatInterval, measureReading } from "../src/readouts.ts";
import { openTickwave, wasm } from "../scripts/node-wasm.ts";

const at = (p: string) => new URL(`../${p}`, import.meta.url);
const ONE = readFileSync(at("fixtures/identify-swd.tickwave"));
const NINE = readFileSync(at("fixtures/identify-swd.4k.tickwave"));
const memory = () => readFixture(readFileSync(at("fixtures/identify-swd.tickwavefx"))).then((r) => new MemorySource(r));

test("the head: what the recording says, from one read", async () => {
  const bytes = new MemoryBytes(ONE, "identify-swd.tickwave");
  const r = await Opened.open(wasm(), bytes);
  assert.deepEqual(bytes.reads, [[0, FIRST_READ]]);
  const i = r.info;
  assert.deepEqual(i.timebase, { num: 1e9, den: 1 });
  assert.equal(i.length, 523583100);
  assert.equal(i.bytes, 4830);
  assert.deepEqual(
    i.channels.map((c) => [c.name, c.kind, c.end]),
    [
      ["SWCLK", "digital", 523583070],
      ["SWDIO", "digital", 523583100],
    ],
  );
});

test("a small first read: the head in two reads, then the block by its range", async () => {
  // The converter reports a 1744-byte head and one block, the rest of the 4830 bytes.
  const bytes = new MemoryBytes(ONE, "identify-swd.tickwave");
  const r = await Opened.open(wasm(), bytes, 512);
  const e = await r.edges(1, 377094000, 377107000);
  assert.deepEqual(bytes.reads, [
    [0, 512],
    [512, 1744],
    [1744, 4830],
  ]);
  assert.equal(e.level, 0);
  assert.deepEqual([...e.ticks], [377094556, 377095556, 377096556, 377097556, 377099556, 377100556, 377101556, 377104078, 377106074]);
});

test("nine blocks: every edge and every level agree with the extractor's fixture", async () => {
  const r = await openTickwave("fixtures/identify-swd.4k.tickwave");
  const m = await memory();
  for (const ch of [0, 1]) {
    const all = await r.edges(ch, 0, r.info.length);
    const want = await m.edges(ch, 0, r.info.length);
    // (122348 - 60) / 8 and (15052 - 60) / 8, from the export's file sizes.
    assert.equal(all.ticks.length, [15286, 1874][ch]);
    assert.deepEqual(all, want);
  }
  // Windows that start and end on each side of every block boundary.
  for (let b = 1; b < r.header.blocks; b++) {
    const t = r.blockStart(b);
    for (const [s, e] of [
      [t - 1, t + 1],
      [t, t + 5000],
      [t - 5000, t],
      [t - 3000, t + 3000],
    ]) {
      for (const ch of [0, 1]) assert.deepEqual(await r.edges(ch, s, e), await m.edges(ch, s, e), `channel ${ch}, [${s}, ${e})`);
    }
  }
});

test("the overview: unpacked from the head, as the extractor's fixture computes it", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const m = await memory();
  // By hand (test/columns.test.ts): SWCLK's buckets of 4096 from 376696832, changing in the 6th
  // and 7th.
  const ov = (await r.overview(0, 376700000, 376725504, 4096))!;
  assert.equal(ov.kind, "digital");
  if (ov.kind !== "digital") return;
  assert.equal(ov.start, 376696832);
  assert.deepEqual([...ov.level], [1, 1, 1, 1, 1, 1, 1]);
  assert.deepEqual([...ov.changes], [0, 0, 0, 0, 0, 1, 1]);
  // Every level the head holds, over the whole capture, against the TypeScript computation.
  assert.deepEqual(r.header.overview.map((s) => 2 ** s), formatLevels(r.info.length));
  assert.equal(r.header.overview_blocks, undefined);
  for (const bucket of formatLevels(r.info.length)) {
    for (const ch of [0, 1]) {
      assert.deepEqual(await r.overview(ch, 0, r.info.length, bucket), await m.overview(ch, 0, r.info.length, bucket), `${bucket}, ${ch}`);
    }
  }
  assert.equal(await r.overview(0, 0, r.info.length, 4095), null);
});

test("overview blocks: a finer level read by range, as the TypeScript computation has it", async () => {
  // The JTAG identify capture without TDO: 626,263,880 ticks, so 2^12 has 152,898 buckets, too
  // many for the head, and sits in three overview blocks.
  const file = readFileSync(at("fixtures/identify-jtag.tickwave"));
  const bytes = new MemoryBytes(file, "identify-jtag.tickwave");
  const r = await Opened.open(wasm(), bytes);
  assert.deepEqual(r.header.overview, [16, 20]);
  assert.deepEqual(r.header.overview_blocks, [{ shift: 12, blocks: 3 }]);
  // The edges, counted against the export's file sizes: (377124 - 60) / 8, (3980 - 60) / 8 and
  // (59308 - 60) / 8.
  const edges = await Promise.all([0, 1, 2].map((ch) => r.edges(ch, 0, r.info.length)));
  assert.deepEqual(edges.map((e) => e.ticks.length), [47133, 490, 7406]);
  const m = new MemorySource({
    title: "",
    timebase: r.info.timebase,
    length: r.info.length,
    channels: r.info.channels.map((c, i) => ({ kind: "digital", name: c.name, end: c.end, initial: edges[i].level, ticks: edges[i].ticks })),
  });
  const reads = bytes.reads.length;
  // A window across the boundary between the first and second overview blocks (bucket 65,536,
  // tick 268,435,456), and the whole capture.
  for (const [s, e] of [
    [260e6, 280e6],
    [0, r.info.length],
  ]) {
    for (const ch of [0, 1, 2]) {
      const got = await r.overview(ch, s, e, 4096);
      assert.equal(got?.bucket, 4096);
      assert.deepEqual(got, await m.overview(ch, s, e, 4096), `[${s}, ${e}), ${ch}`);
    }
  }
  // The window needs overview blocks 0 and 1, one request; the whole capture adds block 2, a
  // second. None is read twice.
  assert.equal(bytes.reads.length, reads + 2);
});

/** A server over a file, honouring Range or not, and recording what it was asked. */
function server(file: Uint8Array, opts: { ranges: boolean; encoding?: string; shift?: number }) {
  const name = file === ONE ? "identify-swd.tickwave" : "identify-swd.4k.tickwave";
  const log: string[] = [];
  const fetchFn: Fetch = async (_url, init) => {
    const range = new Headers(init.headers).get("range");
    log.push(range ?? "whole");
    const m = /^bytes=(\d+)-(\d+)$/.exec(range ?? "");
    if (!opts.ranges || !m) return new Response(file.slice(), { status: 200 });
    const from = Number(m[1]) + (opts.shift ?? 0);
    const to = Math.min(Number(m[2]) + 1, file.length);
    const headers: Record<string, string> = { "content-range": `bytes ${from}-${to - 1}/${file.length}` };
    if (opts.encoding) headers["content-encoding"] = opts.encoding;
    return new Response(file.slice(from, to), { status: 206, headers });
  };
  return { log, bytes: new HttpBytes(`https://example.org/r/${name}`, fetchFn) };
}

test("HTTP: blocks by range; adjacent missing blocks in one request; nothing fetched twice", async () => {
  const s = server(NINE, { ranges: true });
  const r = await Opened.open(wasm(), s.bytes, 1024);
  // The converter reports a 1997-byte head.
  assert.deepEqual(s.log, ["bytes=0-1023", "bytes=1024-1996"]);
  await r.edges(0, 0, r.info.length);
  assert.equal(s.log.length, 3);
  assert.equal(s.log[2], "bytes=1997-6634");
  await r.edges(1, 377094000, 377107000);
  assert.equal(s.log.length, 3);
});

test("HTTP: a file the first read holds whole is not fetched again for its block", async () => {
  const s = server(ONE, { ranges: true });
  const r = await Opened.open(wasm(), s.bytes);
  assert.equal((await r.edges(0, 0, r.info.length)).ticks.length, 15286);
  assert.deepEqual(s.log, [`bytes=0-${FIRST_READ - 1}`]);
});

test("HTTP: a server that ignores Range sends the whole file once, and it serves every read", async () => {
  const s = server(NINE, { ranges: false });
  const r = await Opened.open(wasm(), s.bytes, 1024);
  const all = await r.edges(0, 0, r.info.length);
  assert.equal(all.ticks.length, 15286);
  assert.deepEqual(s.log, ["bytes=0-1023"]);
});

test("HTTP: a compressed range, or the wrong range, is refused", async () => {
  await assert.rejects(Opened.open(wasm(), server(NINE, { ranges: true, encoding: "gzip" }).bytes, 1024), /compresses it \(gzip\)/);
  await assert.rejects(Opened.open(wasm(), server(NINE, { ranges: true, shift: 7 }).bytes, 1024), /asked for bytes from 0, got 7/);
});

/** Bytes whose reads wait until released, and honour their signal meanwhile. */
class SlowBytes implements Bytes {
  readonly name = "slow";
  reads = 0;
  aborts = 0;
  #inner: Bytes;
  #release: (() => void)[] = [];
  constructor(inner: Bytes) {
    this.#inner = inner;
  }
  release() {
    for (const r of this.#release.splice(0)) r();
  }
  async read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array> {
    this.reads++;
    await new Promise<void>((resolve, reject) => {
      this.#release.push(resolve);
      // A real fetch rejects a moment after its abort, not at once, and never resolves after it.
      signal?.addEventListener("abort", () => {
        this.aborts++;
        this.#release = this.#release.filter((r) => r !== resolve);
        setTimeout(() => reject(new DOMException("aborted", "AbortError")), 20);
      });
    });
    return this.#inner.read(from, to);
  }
}

test("an abort stops the request at once, and the read no other request waits on", async () => {
  const head = (await openTickwave("fixtures/identify-swd.4k.tickwave")).header;
  const slow = new SlowBytes(new MemoryBytes(NINE, "identify-swd.4k.tickwave"));
  // The head comes at once; every block read waits. head_len is the preamble's u32 at 20.
  const headLen = new DataView(NINE.buffer, NINE.byteOffset).getUint32(20, true);
  const s = await Opened.open(wasm(), { name: "slow", read: (a, b, sig) => (a === 0 ? new MemoryBytes(NINE, "x").read(0, headLen) : slow.read(a, b, sig)) });
  assert.equal(s.header.blocks, head.blocks);
  const c = new AbortController();
  const first = s.edges(0, 0, s.info.length, c.signal);
  const second = s.edges(1, 0, s.info.length).catch((e: unknown) => e);
  c.abort();
  await assert.rejects(first, (e) => isAbort(e));
  // The second request still waits on the read, so it was not aborted.
  assert.equal(slow.aborts, 0);
  slow.release();
  const got = await second;
  assert.ok(!(got instanceof Error) && !isAbort(got), `the second request failed: ${got}`);
  assert.equal((got as { ticks: Float64Array }).ticks.length, 1874);
  // Alone on a read, an abort stops it.
  setCacheBytes(1);
  const d = new AbortController();
  const third = s.edges(0, 0, s.info.length, d.signal);
  d.abort();
  await assert.rejects(third, (e) => isAbort(e));
  assert.equal(slow.aborts, 1);
  // A request that arrives before the aborted read has rejected starts a read of its own,
  // instead of joining the dead one.
  const fourth = s.edges(0, 0, s.info.length).catch((e: unknown) => e);
  await new Promise((r) => setTimeout(r, 0));
  slow.release();
  const last = await fourth;
  assert.ok(!isAbort(last), "the next request joined the aborted read");
  assert.equal((last as { ticks: Float64Array }).ticks.length, 15286);
  setCacheBytes(192 << 20);
});

test("the block cache: an evicted block is read again", async () => {
  const bytes = new MemoryBytes(NINE, "identify-swd.4k.tickwave");
  const r = await Opened.open(wasm(), bytes, 1024);
  setCacheBytes(1);
  await r.edges(0, 0, r.info.length);
  await r.edges(0, 0, r.info.length);
  setCacheBytes(192 << 20);
  await r.edges(0, 0, r.info.length);
  await r.edges(0, 0, r.info.length);
  // Two head reads; two full reads while the cache held nothing; one after it held everything.
  assert.equal(bytes.reads.length, 2 + 2 + 1);
});

test("the sidecar resolves through the Rust core, and its problems come back", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const hand = handJson(readFileSync(at("fixtures/identify-swd.annotations.toml"), "utf8"), "identify-swd.annotations.toml").json;
  const jsonl = execFileSync(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave"], { encoding: "utf8" });
  const list = JSON.parse(r.resolveAnnotations(hand, ["identify-swd.swd.jsonl"], [jsonl], "identify-swd.tickwave")) as { id: string; start: number; end: number | null }[];
  // 687 generated by the decoder, 8 hand-written.
  assert.equal(list.length, 695);
  const read = list.find((a) => a.id === "dpidr-read")!;
  assert.deepEqual([read.start, read.end], [377094557, 377140560]);
  assert.ok(list.some((a) => a.id === "swd/wait/379065159"));
  assert.throws(() => r.resolveAnnotations(hand, ["identify-swd.swd.jsonl"], [jsonl], "other.tickwave"), /names "identify-swd.tickwave", not "other.tickwave"/);
  const stale = jsonl.replace(/blake3:[0-9a-f]+/, "blake3:00");
  assert.throws(() => r.resolveAnnotations(hand, ["identify-swd.swd.jsonl"], [stale], "identify-swd.tickwave"), /generated for blake3:00/);
});

test("a TOML float where a time belongs is refused, as the CLI refuses it", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const toml = readFileSync(at("fixtures/identify-swd.annotations.toml"), "utf8");
  const jsonl = execFileSync(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave"], { encoding: "utf8" });
  const site = 'start = "376.719436 ms"';
  assert.equal(toml.split(site).length, 2);
  const resolve = (start: string) => () =>
    r.resolveAnnotations(handJson(toml.replace(site, `start = ${start}`), "hand.toml").json, ["identify-swd.swd.jsonl"], [jsonl], "identify-swd.tickwave");
  resolve("376719436")();
  assert.throws(resolve("376719436.0"), /did not match any variant of untagged enum Time/);
  assert.throws(resolve("9007199254740993"), /cannot hold it exactly/);
});

test("values pass through the browser's TOML route as strings, and a number in their place is refused", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const toml = readFileSync(at("fixtures/identify-swd.annotations.toml"), "utf8");
  const jsonl = execFileSync(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave"], { encoding: "utf8" });
  const site = 'kind = "attach"';
  assert.equal(toml.split(site).length, 2);
  const resolve = (values: string) => () =>
    r.resolveAnnotations(handJson(toml.replace(site, `${site}\nvalues = ${values}`), "hand.toml").json, ["identify-swd.swd.jsonl"], [jsonl], "identify-swd.tickwave");
  const list = JSON.parse(resolve('{ clock = "1.0 MHz", reads = "3 reads" }')()) as { id: string; values: Record<string, string> | null }[];
  assert.deepEqual(list.find((a) => a.id === "attach")!.values, { clock: "1.0 MHz", reads: "3 reads" });
  assert.equal(list.find((a) => a.id === "first-wait")!.values, null);
  assert.throws(resolve('{ clock = "1.0MHz" }'), /clock = "1.0MHz" is not a decimal and a unit/);
  assert.throws(resolve("{ clock = 1.0 }"), /expected a string/);
  assert.throws(resolve("{ clock = 1 }"), /expected a string/);
});

/** A Saleae analog export's f32 volts (one chunk), read here without the Rust core. */
function saleaeVolts(path: string): Float32Array {
  const b = readFileSync(path);
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  assert.equal(b.subarray(0, 8).toString("latin1"), "<SALEAE>");
  assert.equal(v.getInt32(12, true), 1, "an analog export");
  const count = Number(v.getBigInt64(56, true));
  return new Float32Array(b.buffer.slice(b.byteOffset + 64, b.byteOffset + 64 + 4 * count));
}

/** An analog channel from samples, for the TypeScript overview. */
function analogSource(r: Opened, channels: { name: string; first: number; values: Float32Array }[]) {
  return new MemorySource({
    title: "",
    timebase: r.info.timebase,
    length: r.info.length,
    channels: channels.map((c) => ({
      kind: "analog" as const,
      name: c.name,
      end: c.first + c.values.length,
      unit: "V",
      first: c.first,
      period: { num: 1, den: 1 },
      resolution: null,
      values: Float64Array.from(c.values),
    })),
  });
}

const numbers = (a: ArrayLike<number>) => Array.from(a);

test("analog: every sample is the export's value, and the channel ends with its samples", async () => {
  const r = await openTickwave("fixtures/swd50-cut.tickwave");
  const [clk, dio, nrst] = r.info.channels;
  assert.ok(clk.kind === "analog" && dio.kind === "analog" && nrst.kind === "digital");
  // 50,000 samples at 1 GS/s from tick 0; the capture runs on to NRST's end, 11,031,886.
  assert.deepEqual([clk.first, clk.count, clk.end, clk.unit], [0, 50000, 50000, "V"]);
  assert.equal(nrst.end, 11031886);
  assert.equal(r.info.length, 11031886);
  for (const [ch, file] of [
    [0, "analog_0"],
    [1, "analog_1"],
  ] as const) {
    const want = saleaeVolts(`../testdata/saleae/swd50-cut/${file}.bin`);
    const got = await r.samples(ch, 0, r.info.length);
    assert.equal(got.first, 0);
    assert.deepEqual(numbers(got.values), numbers(want));
  }
  // A window: ticks [1020, 1061) are samples 1020 to 1060.
  const w = await r.samples(0, 1020, 1061);
  assert.equal(w.first, 1020);
  assert.equal(w.values.length, 41);
  assert.equal(w.values[0], 3.26922607421875);
});

test("analog: the head's overview, as the TypeScript computation has it from the samples", async () => {
  const r = await openTickwave("fixtures/swd50-cut.tickwave");
  assert.deepEqual(r.header.overview, [12, 16]);
  const values = await Promise.all([0, 1].map((ch) => r.samples(ch, 0, r.info.length)));
  const m = analogSource(r, [
    { name: "SWCLK", first: 0, values: values[0].values as Float32Array },
    { name: "SWDIO", first: 0, values: values[1].values as Float32Array },
  ]);
  for (const bucket of [4096, 65536]) {
    for (const ch of [0, 1]) {
      const got = (await r.overview(ch, 0, r.info.length, bucket))!;
      const want = (await m.overview(ch, 0, r.info.length, bucket))!;
      assert.equal(got.kind, "analog");
      if (got.kind !== "analog" || want.kind !== "analog") return;
      assert.deepEqual([got.bucket, got.start, numbers(got.min), numbers(got.max)], [want.bucket, want.start, numbers(want.min), numbers(want.max)], `${bucket}, ${ch}`);
    }
  }
});

const SESSION50 = new URL("../fixtures-local/session50.tickwave", import.meta.url);

test("analog overview blocks: session50's 2^12 level, as the TypeScript computation has it", { skip: !existsSync(SESSION50) && "no fixtures-local/session50.tickwave" }, async () => {
  const r = await openTickwave(SESSION50.pathname);
  assert.deepEqual(r.header.overview_blocks, [{ shift: 12, blocks: 5 }]);
  // The analog samples end at tick 100,002,000, inside overview block 0, though the capture runs
  // to 1.26 s. Two windows of whole buckets: one inside the samples, one across their end.
  for (const [s, e] of [
    [49 * 2 ** 20, 50 * 2 ** 20],
    [95 * 2 ** 20, 96 * 2 ** 20],
  ]) {
    for (const ch of [0, 1]) {
      const c = r.info.channels[ch];
      assert.ok(c.kind === "analog" && c.period.num === 1 && c.period.den === 1 && c.first === 0);
      assert.equal(c.end, 100002000);
      const values = await r.samples(ch, s, e);
      const m = analogSource(r, [{ name: c.name, first: values.first, values: values.values as Float32Array }]);
      const got = (await r.overview(ch, s, e, 4096))!;
      const want = (await m.overview(0, s, e, 4096))!;
      assert.ok(got.kind === "analog" && want.kind === "analog");
      assert.equal(got.bucket, 4096);
      // Not vacuous: the buckets hold samples.
      assert.ok(got.min.length > 0 && numbers(got.min).every(Number.isFinite), `[${s}, ${e}) has samples`);
      assert.deepEqual([got.start, numbers(got.min), numbers(got.max)], [want.start, numbers(want.min), numbers(want.max)], `[${s}, ${e}), channel ${ch}`);
    }
  }
});

test("duration: first edge to last, of either polarity, of the edges in [start, end)", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const tb = r.info.timebase;
  const duration = async (ch: number, s: number, e: number) => {
    const edges = await r.edges(ch, s, e);
    return { edges, reading: measureReading("duration", edges.level, edges.ticks, tb) };
  };
  // The DPIDR read on SWCLK: its first edge falls at 377094557, its span's start; its last rises
  // at 377140060 (the independent decode). 45,503 ticks.
  const read = await duration(0, 377094557, 377140560);
  const span = read.edges.ticks[read.edges.ticks.length - 1] - read.edges.ticks[0];
  assert.equal(span, 45503);
  assert.deepEqual(read.reading, { value: formatInterval(span, tb), note: "first edge to last" });
  assert.equal(read.reading!.value, "45.503 µs");
  // Ending exactly on that last rising edge leaves it out: the last edge is the fall at
  // 377139560, 45,003 ticks from the first.
  const cut = await duration(0, 377094557, 377140060);
  assert.equal(cut.edges.ticks[cut.edges.ticks.length - 1], 377139560);
  assert.equal(cut.reading!.value, "45.003 µs");
  // One edge says nothing: SWDIO in [377104000, 377105000) holds only 377104078.
  assert.equal((await duration(1, 377104000, 377105000)).reading, null);
  assert.equal(measureReading("width", 0, [1, 2], tb), undefined);
});
