// The WASM connector in Node: the same reader and recording code the worker runs, over the real
// decoder. Expected values come from the Saleae export (file sizes, an independent decode), from
// the converter's own report, or from the .tickwavefx fixture, which my extractor wrote from the
// export without the Rust core: two pipelines that must agree. Which bytes a reader fetches comes
// from test/layout.ts, which reads a file's top index and pages by FORMAT.md without the decoder.

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HttpBytes, MemoryBytes, type Bytes, type Fetch } from "../src/wasm/bytes.ts";
import { Opened, setCacheBytes } from "../src/wasm/opened.ts";
import { handJson } from "../src/wasm/hand.ts";
import { FIRST_READ } from "../src/wasm/protocol.ts";
import { readFixture } from "../src/fixture-file.ts";
import { formatLevels, MemorySource } from "../src/memory-source.ts";
import { isAbort } from "../src/source.ts";
import { formatInterval, measureReading, ONE as STEP_ONE, periodStats } from "../src/readouts.ts";
import { openTickwave, wasm } from "../scripts/node-wasm.ts";
import { blockRows, layout as tables, leaves, levelRows, range } from "./layout.ts";
import { sortedStats } from "./sorted-periods.ts";

const at = (p: string) => new URL(`../${p}`, import.meta.url);
const ONE = readFileSync(at("fixtures/identify-swd.tickwave"));
const NINE = readFileSync(at("fixtures/identify-swd.4k.tickwave"));
const memory = () => readFixture(readFileSync(at("fixtures/identify-swd.tickwavefx"))).then((r) => new MemorySource(r));

/** Where a file's footer starts, as its trailer says, and where channel `c`'s block page and its
 * blocks' frames lie. */
function layout(file: Uint8Array, c = 0) {
  const t = tables(file);
  const blocks = t.channels[c].blocks!;
  return { footer: t.footer, page: range(leaves(file, blocks)[0]), blocks: blockRows(file, blocks).map((r) => [r.from, r.to] as [number, number]) };
}

/** A channel's finest overview shift, as its header entry or the recording's states it. */
function finest(file: Uint8Array, c: number): number {
  const h = tables(file).header as unknown as { overview_shift: number; channels: { overview_shift?: number }[] };
  return h.channels[c].overview_shift ?? h.overview_shift;
}

test("the footer: what the recording says, from one read at the end", async () => {
  const bytes = new MemoryBytes(ONE, "identify-swd.tickwave");
  const r = await Opened.open(wasm(), bytes);
  // The file is shorter than the first read: it comes whole.
  assert.ok(ONE.length < FIRST_READ);
  assert.deepEqual(bytes.reads, [[0, ONE.length]]);
  const i = r.info;
  assert.deepEqual(i.timebase, { num: 1e9, den: 1 });
  assert.equal(i.length, 523583100);
  assert.equal(i.bytes, ONE.length);
  assert.deepEqual(
    i.channels.map((c) => [c.name, c.kind, c.end]),
    [
      ["SWCLK", "digital", 523583070],
      ["SWDIO", "digital", 523583100],
    ],
  );
});

test("the footer is parsed once: footerStart reads only the trailer, and the Recording the footer", () => {
  const { footerStart, Recording } = wasm();
  const { footer } = layout(ONE);
  // A byte of the footer's header changed: the trailer still says where the footer starts.
  const bad = Uint8Array.from(ONE);
  bad[footer + 5 + 8 + 2] ^= 0x20;
  assert.equal(footerStart(bad, bad.length), 0);
  assert.equal(footerStart(bad.subarray(bad.length - 512), bad.length), footer);
  assert.throws(() => new Recording(bad, bad.length), /footer record is not whole/);
  // A tail too short for the trailer asks for the trailer; one that is no trailer is refused.
  assert.equal(footerStart(bad.subarray(bad.length - 10), bad.length), bad.length - 24);
  assert.throws(() => footerStart(bad.subarray(0, bad.length - 1), bad.length - 1), /no trailer at the end/);
});

test("a first draw reads only the trailer, the footer and the strips of the channels it shows", async () => {
  // swd50-cut's footer lies in its last 32 KiB, with pages and overview blocks before it.
  const file = readFileSync(at("fixtures/swd50-cut.tickwave"));
  const bytes = new MemoryBytes(file, "swd50-cut.tickwave");
  const r = await Opened.open(wasm(), bytes);
  assert.deepEqual(bytes.reads, [[file.length - FIRST_READ, file.length]]);
  assert.ok(tables(file).footer > file.length - FIRST_READ);
  for (const [c, ch] of r.info.channels.entries()) {
    const strip = (await r.overview(c, 0, ch.end, Infinity))!;
    // The strip, a level coarser than the overview blocks below it.
    assert.equal(strip.bucket, ch.kind === "analog" ? 2 ** 14 : 2 ** 62);
  }
  assert.equal(bytes.reads.length, 1);
  // The exact answer reads on: SWCLK's 2^6 level's page and its rows.
  await r.overview(0, 0, r.info.length, 64);
  assert.ok(bytes.reads.length > 1);
});

test("a query reads only its channel's page and the blocks of its span", async () => {
  // energy: V, I and the pins each have blocks of their own, six of V and of I.
  const file = readFileSync(at("fixtures/energy.tickwave"));
  const bytes = new MemoryBytes(file, "energy.tickwave");
  const r = await Opened.open(wasm(), bytes, 2048);
  const opened = bytes.reads.length;
  const t = tables(file);
  const rows = blockRows(file, t.channels[1].blocks!);
  // I's samples from 1,008,000 (after the loss) to 1,440,000 lie in its block from 960,000 to
  // 1,440,000 alone.
  const s = await r.samples(1, 1_008_000, 1_440_000);
  assert.equal(s.first, 1000);
  assert.deepEqual(rows[2].start, 960_000);
  assert.deepEqual(bytes.reads.slice(opened), [range(leaves(file, t.channels[1].blocks!)[0]), [rows[2].from, rows[2].to]]);
});

test("a bundled sidecar is read when first asked for, once", async () => {
  const file = readFileSync(at("fixtures/energy.tickwave"));
  const bytes = new MemoryBytes(file, "energy.tickwave");
  // A first read shorter than the footer, which starts after the bundle.
  const r = await Opened.open(wasm(), bytes, 2048);
  const { bundle, footer } = tables(file);
  assert.deepEqual(bytes.reads, [[file.length - 2048, file.length], [footer, file.length - 2048]]);
  const files = await r.bundle();
  assert.deepEqual(Object.keys(files ?? {}).sort(), ["energy.annotations.toml", "energy.verge.jsonl"]);
  assert.deepEqual(bytes.reads.slice(2), [bundle]);
  await r.bundle();
  assert.equal(bytes.reads.length, 3);
  // A recording that bundles none reads nothing for it.
  const one = new MemoryBytes(ONE, "identify-swd.tickwave");
  assert.equal(await (await Opened.open(wasm(), one)).bundle(), null);
  assert.equal(one.reads.length, 1);
});

test("a small first read: the footer in two reads from the end, then the channel's index page and its block by range", async () => {
  const bytes = new MemoryBytes(ONE, "identify-swd.tickwave");
  const { footer, page, blocks } = layout(ONE, 1);
  assert.ok(footer < ONE.length - 512, "a footer longer than the first read");
  const r = await Opened.open(wasm(), bytes, 512);
  const e = await r.edges(1, 377094000, 377107000);
  // SWDIO's page and its one block; nothing of SWCLK's.
  assert.deepEqual(bytes.reads, [[ONE.length - 512, ONE.length], [footer, ONE.length - 512], page, blocks[0]]);
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
  // Windows that start and end on each side of every block boundary of each channel: SWCLK's
  // eight blocks, SWDIO's one.
  const t = tables(NINE);
  const starts = [0, 1].map((ch) => blockRows(NINE, t.channels[ch].blocks!).map((b) => b.start));
  assert.deepEqual(starts.map((s) => s.length), [8, 1]);
  for (const ch of [0, 1]) {
    for (const b of starts[ch].slice(1)) {
      for (const [s, e] of [
        [b - 1, b + 1],
        [b, b + 5000],
        [b - 5000, b],
        [b - 3000, b + 3000],
      ]) {
        for (const c of [0, 1]) assert.deepEqual(await r.edges(c, s, e), await m.edges(c, s, e), `channel ${c}, [${s}, ${e})`);
      }
    }
  }
});

test("an edge count comes from the index and the blocks at the span's ends, exactly, reading no block between", async () => {
  const m = await memory();
  const t = tables(NINE);
  const rows = blockRows(NINE, t.channels[0].blocks!);
  // SWCLK's eight blocks: spans that start and end on each side of each boundary, a block's
  // start, the capture's ends, and past them.
  const ticks = [-5, 0, 1, ...rows.slice(1).flatMap((b) => [b.start - 1, b.start, b.start + 1]), 523583069, 523583100, 523583200];
  const r = await openTickwave("fixtures/identify-swd.4k.tickwave");
  let counted = 0;
  for (const s of ticks) {
    for (const e of ticks) {
      if (e < s) continue;
      for (const c of [0, 1]) assert.equal(await r.edgeCount(c, s, e), await m.edgeCount(c, s, e), `channel ${c}, [${s}, ${e})`);
      counted++;
    }
  }
  // 27 ticks, every pair in order.
  assert.equal(counted, (27 * 28) / 2);
  // Not vacuous: the whole capture's counts.
  assert.deepEqual([await r.edgeCount(0, 0, 523583100), await r.edgeCount(1, 0, 523583100)], [15286, 1874]);
  // From the second block's middle to the seventh's: those two blocks' frames are read, and no
  // other's.
  const bytes = new MemoryBytes(NINE, "identify-swd.4k.tickwave");
  const fresh = await Opened.open(wasm(), bytes, NINE.length - t.footer);
  const before = bytes.reads.length;
  const [a, b] = [(rows[1].start + rows[2].start) / 2, (rows[6].start + rows[7].start) / 2];
  assert.equal(await fresh.edgeCount(0, a, b), await m.edgeCount(0, a, b));
  const frames = rows.map((row) => [row.from, row.to] as const);
  const touched = frames.map(([from, to]) => bytes.reads.slice(before).some(([x, y]) => x < to && y > from));
  assert.deepEqual(touched, [false, true, false, false, false, false, true, false]);
});

test("a span's periods are worked out beside its edges, as periodStats gives them from the edges", async () => {
  const m = await memory();
  const rows = blockRows(NINE, tables(NINE).channels[0].blocks!);
  const r = await openTickwave("fixtures/identify-swd.4k.tickwave");
  // Spans that start and end either side of each of SWCLK's block boundaries, and the capture's ends.
  const ticks = [0, ...rows.slice(1).flatMap((b) => [b.start - 1, b.start + 1]), 523583100];
  let spans = 0;
  for (const s of ticks) {
    for (const e of ticks) {
      if (e <= s) continue;
      for (const c of [0, 1]) {
        const want = await m.edges(c, s, e);
        assert.deepEqual(await r.periods(c, s, e), periodStats(want.level, want.ticks), `channel ${c}, [${s}, ${e})`);
      }
      spans++;
    }
  }
  assert.equal(spans, (16 * 15) / 2);
  // Not vacuous: the whole capture's 7,643 rising SWCLK edges give 7,642 periods, the export's
  // count; a span with no edge gives none.
  assert.equal((await r.periods(0, 0, 523583100))?.count, 7642);
  assert.equal(await r.periods(0, 0, 1), null);
});

test("a span's periods, a group of blocks at a time, are the sorted figures of its edges joined, over random spans", async () => {
  const r = await openTickwave("fixtures/identify-swd.4k.tickwave");
  let seed = 9;
  const next = () => (seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31;
  let blocks = 0;
  for (let i = 0; i < 300; i++) {
    const s = Math.floor(next() * 523583100);
    const e = s + Math.floor(next() * next() * 300_000_000);
    for (const c of [0, 1]) {
      const joined = await r.edges(c, s, e);
      assert.deepEqual(await r.periods(c, s, e), sortedStats(joined.level, joined.ticks), `channel ${c}, [${s}, ${e})`);
    }
    blocks += Number(e - s > 40_000_000);
  }
  // Not vacuous: many spans cross several of the 4 KiB blocks.
  assert.ok(blocks > 100, `${blocks}`);
});

test("a span's periods read its blocks a group at a time, each group's in one read, on both passes", async (t) => {
  // SWCLK's eight blocks: their first edges at 0, 2,039, 4,079, 6,122, 8,168, 10,214, 12,260 and
  // 14,304 of 15,286, their frames at bytes 524-894, 935-1265, 1306-1635, 1676-1924, 1965-2231,
  // 2272-2537, 2578-2921 and 2962-3165. A cache that holds no block, so the second pass reads too.
  setCacheBytes(1);
  t.after(() => setCacheBytes(192 << 20));
  const joined = await (await openTickwave("fixtures/identify-swd.4k.tickwave")).edges(0, 0, 523583100);
  const want = sortedStats(joined.level, joined.ticks);
  const footer = tables(NINE).footer;
  const reads = async (groupEdges?: number) => {
    const bytes = new MemoryBytes(NINE, "identify-swd.4k.tickwave");
    const r = await Opened.open(wasm(), bytes, NINE.length - footer);
    const before = bytes.reads.length;
    assert.deepEqual(await r.periods(0, 0, 523583100, undefined, { groupEdges }), want);
    return bytes.reads.slice(before).filter(([a, b]) => a < 3165 && b > 524);
  };
  // All of them one group: a read a pass.
  assert.deepEqual(await reads(), [
    [524, 3165],
    [524, 3165],
  ]);
  // Groups of at most 4,100 edges: two blocks each.
  const pairs = [
    [524, 1265],
    [1306, 1924],
    [1965, 2537],
    [2578, 3165],
  ];
  assert.deepEqual(await reads(4_100), [...pairs, ...pairs]);
  // Of one edge: each block alone.
  assert.equal((await reads(1)).length, 16);
});

test("the burst's 10 million periods, a few distinct values, are counted exactly: the sorted figures, its edges never joined", async () => {
  // web/fixtures/burst.tickwave's D, as crates/format/examples/burst.rs writes it: an edge every
  // 10 s, and 20,000,000 edges 2 ticks apart from 30 minutes and a tick, low before the first.
  const [hz, length, at, burst] = [1e9, 3_600e9, 1_800e9 + 1, 20_000_000];
  const ticks: number[] = [];
  for (let t = 10 * hz; t < at; t += 10 * hz) ticks.push(t);
  const before = ticks.length;
  const all = new Float64Array(before + burst + 200);
  all.set(ticks);
  let n = before;
  for (let i = 0; i < burst; i++) all[n++] = at + 2 * i;
  for (let t = Math.ceil((at + 2 * burst) / (10 * hz)) * 10 * hz; t < length; t += 10 * hz) all[n++] = t;
  const want = sortedStats(0, all.subarray(0, n))!;
  const r = await openTickwave("fixtures/burst.tickwave");
  assert.deepEqual(await r.periods(0, 0, length), want);
  // Not vacuous: more periods than the distinct ones counted at most, and the burst's period the
  // median.
  assert.ok(want.count > 2 ** 21 && want.median === 4, JSON.stringify(want));
});

test("past the distinct periods a span counts, here 8, its count, least, most and mean stay exact, and it has no median", async () => {
  const r = await openTickwave("fixtures/identify-swd.4k.tickwave");
  const joined = await r.edges(0, 0, 523583100);
  const want = sortedStats(joined.level, joined.ticks)!;
  const got = await r.periods(0, 0, 523583100, undefined, { distinctAtMost: 8 });
  // SWCLK's periods are rising edge to rising edge: their ends, the first and the last.
  const rising = Array.from(joined.ticks).filter((_, i) => (joined.level ^ (i & 1)) === 0);
  assert.deepEqual(got, { count: want.count, median: null, min: want.min, max: want.max, used: want.count, runs: 1, span: rising[rising.length - 1] - rising[0] });
  assert.ok(want.median !== null);
});

test("every analog and derived channel's coarsest overview holds its least and greatest values: those of all its samples, exactly", async () => {
  // A row's whole range comes from that overview (track.ts `wholeRange`), not from the info.
  let checked = 0;
  for (const name of readdirSync(at("fixtures")).filter((f) => f.endsWith(".tickwave"))) {
    const r = await openTickwave(`fixtures/${name}`);
    for (const [c, ch] of r.info.channels.entries()) {
      if (ch.kind !== "analog") continue;
      const strip = await r.overview(c, 0, r.info.length, Infinity);
      assert.equal(strip?.kind, "analog", `${name} ${ch.name}`);
      if (strip?.kind !== "analog") continue;
      const { values } = await r.samples(c, 0, r.info.length);
      let [lo, hi] = [Infinity, -Infinity];
      for (const v of values) {
        if (v < lo) lo = v;
        if (v > hi) hi = v;
      }
      let [slo, shi] = [Infinity, -Infinity];
      for (const v of strip.min) if (v < slo) slo = v;
      for (const v of strip.max) if (v > shi) shi = v;
      assert.ok(Object.is(slo, lo) && Object.is(shi, hi), `${name} ${ch.name}: ${slo}..${shi} against ${lo}..${hi}`);
      checked++;
    }
  }
  // Not vacuous: energy's V, I and P, swd50-cut's two, phased's A and B, unavailable's readable ones.
  assert.ok(checked >= 7, `${checked}`);
});

test("the overview: each channel's own levels, unpacked from the footer, as the extractor's fixture computes them", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  // The converter's finest level puts about 64 of a channel's edges in a bucket. SWCLK: 15,286
  // edges over 523,583,070 ticks, 34,252 ticks apart, so round(log2(64 × 34,252)) = 21. SWDIO:
  // 1,874 over 523,583,100, 279,393 apart, so round(log2(64 × 279,393)) = 24. Each is the
  // channel's strip: 250 and 32 buckets, at most 1,024.
  assert.deepEqual([finest(ONE, 0), finest(ONE, 1)], [21, 24]);
  const ov = (await r.overview(0, 0, r.info.length, 2 ** 21))!;
  assert.ok(ov.kind === "digital");
  assert.deepEqual([ov.start, ov.bucket, ov.density.length, ov.duty.length], [0, 2 ** 21, 250, 250]);
  // Every level of each channel, over the whole capture, against the TypeScript computation.
  const fixture = await readFixture(readFileSync(at("fixtures/identify-swd.tickwavefx")));
  for (const ch of [0, 1]) {
    const levels = formatLevels(r.info.length, finest(ONE, ch));
    assert.deepEqual(levels, [2 ** [21, 24][ch]]);
    const m = new MemorySource(fixture, { levels });
    for (const bucket of levels) assert.deepEqual(await r.overview(ch, 0, r.info.length, bucket), await m.overview(ch, 0, r.info.length, bucket), `${bucket}, ${ch}`);
    // Not vacuous: some of each channel's buckets hold edges and some none, and some lie wholly
    // at one level and some between.
    const got = (await r.overview(ch, 0, r.info.length, levels[0]))!;
    assert.ok(got.kind === "digital" && got.density.includes(0) && got.density.some((d) => d > 0), `${ch}`);
    assert.ok(got.duty.some((d) => d === 0 || d === 14) && got.duty.some((d) => d > 0 && d < 14), `${ch}`);
  }
  assert.equal(await r.overview(0, 0, r.info.length, 2 ** 21 - 1), null);
  assert.equal(await r.overview(1, 0, r.info.length, 2 ** 24 - 1), null);
});

test("overview blocks: a finer level read by range, each channel's own, as the TypeScript computation has it", async () => {
  // The JTAG identify capture without TDO: 626,263,880 ticks, so 2^12 has 152,898 buckets, too
  // many for a strip. Its file declares 2^12 as its finest level (web/README.md, "Fixtures"), so
  // each channel's 2^12 level sits in 38 overview blocks of 4,096 buckets and its 2^16 level in 3,
  // and its strip is 2^20.
  const file = readFileSync(at("fixtures/identify-jtag.tickwave"));
  const bytes = new MemoryBytes(file, "identify-jtag.tickwave");
  const r = await Opened.open(wasm(), bytes);
  const t = tables(file);
  for (const ch of [0, 1, 2]) {
    assert.deepEqual(formatLevels(r.info.length, finest(file, ch)), [2 ** 12, 2 ** 16, 2 ** 20]);
    assert.deepEqual(
      r.header.channels[ch].overview_blocks?.map((l) => [l.shift, l.blocks]),
      [
        [12, 38],
        [16, 3],
      ],
    );
  }
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
  // A window across the boundary between the 16th and 17th overview blocks (bucket 65,536, tick
  // 268,435,456), and the whole capture.
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
  // Each overview block spans 4,096 × 4,096 ticks. The window needs blocks 15 and 16, which each
  // channel's first row stands for, with 20 more alike (a quiet stretch): for each channel its
  // level's page, then that row's frame. The whole capture adds the channel's other rows, which
  // lie in two stretches of the file, a row within 30 bytes of the one before: a read a stretch.
  // None is read twice, and none of another level or of the blocks.
  const rows = [0, 1, 2].map((ch) => levelRows(file, t.channels[ch].levels[0]));
  const stretches = [
    [1, 6, 7, 8],
    [1, 2, 3, 4],
    [1, 6, 7, 8],
  ];
  for (const [ch, [a, b, c, d]] of stretches.entries()) {
    assert.equal(rows[ch].length, d + 1);
    assert.deepEqual([rows[ch][0].k, rows[ch][0].n], [0, 22]);
    for (let i = a; i < d; i++) assert.ok(rows[ch][i + 1].from - rows[ch][i].to <= (i === b ? Infinity : 30), `channel ${ch}, row ${i}`);
    assert.ok(rows[ch][c].from - rows[ch][b].to > 512);
  }
  const page = (ch: number) => range(leaves(file, t.channels[ch].levels[0])[0]);
  assert.deepEqual(bytes.reads.slice(reads), [
    ...[0, 1, 2].flatMap((ch) => [page(ch), [rows[ch][0].from, rows[ch][0].to]]),
    ...stretches.flatMap(([a, b, c, d], ch) => [
      [rows[ch][a].from, rows[ch][b].to],
      [rows[ch][c].from, rows[ch][d].to],
    ]),
  ]);
});

/** A server over a file, honouring Range or not, and recording what it was asked. `other` serves
 * it as GitHub Pages does to a page on another origin: a read from the end needs a preflight,
 * which it refuses, and `Content-Range` is hidden; `short` drops each ranged answer's last byte. */
function server(file: Uint8Array, opts: { ranges: boolean; encoding?: string; shift?: number; other?: boolean; short?: boolean; noLength?: boolean }) {
  const name = file === ONE ? "identify-swd.tickwave" : "identify-swd.4k.tickwave";
  const log: string[] = [];
  let aborted = 0;
  const fetchFn: Fetch = async (_url, init) => {
    const range = new Headers(init.headers).get("range");
    log.push(range ?? "whole");
    init.signal?.addEventListener("abort", () => void aborted++);
    const m = /^bytes=(\d+)-(\d*)$/.exec(range ?? "");
    const end = /^bytes=-(\d+)$/.exec(range ?? "");
    if (opts.other && end) throw new TypeError("Failed to fetch");
    if (!opts.ranges || !(m || end)) return new Response(file.slice(), { status: 200 });
    // A suffix longer than the file: 416 and its size, as GitHub Pages answers it.
    if (end && Number(end[1]) > file.length) return new Response(null, { status: 416, headers: { "content-range": `bytes */${file.length}` } });
    if (m && Number(m[1]) >= file.length) return new Response(null, { status: 416, ...(!opts.other && { headers: { "content-range": `bytes */${file.length}` } }) });
    const from = m ? Number(m[1]) + (opts.shift ?? 0) : file.length - Number(end![1]);
    const to = m && m[2] ? Math.min(Number(m[2]) + 1, file.length) : file.length;
    const body = file.slice(from, opts.short ? to - 1 : to);
    const headers: Record<string, string> = opts.other ? {} : { "content-range": `bytes ${from}-${to - 1}/${file.length}` };
    if (!opts.noLength) headers["content-length"] = String(to - from);
    if (opts.encoding) headers["content-encoding"] = opts.encoding;
    return new Response(body, { status: 206, headers });
  };
  const bytes = new HttpBytes(`https://example.org/r/${name}`, fetchFn, opts.other ? "https://page.example" : undefined);
  return { log, bytes, aborted: () => aborted };
}

test("HTTP: the end first, a channel's page and blocks by range, adjacent blocks in one request, nothing fetched twice", async () => {
  // A first read shorter than the footer's 820 bytes, so the footer takes a second.
  const s = server(NINE, { ranges: true });
  const { footer, page, blocks } = layout(NINE, 0);
  const swdio = layout(NINE, 1);
  const r = await Opened.open(wasm(), s.bytes, 512);
  assert.deepEqual(s.log, ["bytes=-512", `bytes=${footer}-${NINE.length - 512 - 1}`]);
  // SWCLK's page, then its eight blocks, which lie one after another: one request.
  await r.edges(0, 0, r.info.length);
  assert.deepEqual(s.log.slice(2), [`bytes=${page[0]}-${page[1] - 1}`, `bytes=${blocks[0][0]}-${blocks[blocks.length - 1][1] - 1}`]);
  // SWDIO's page and its block, a request each; SWCLK's are not asked again.
  await r.edges(1, 377094000, 377107000);
  await r.edges(0, 377094000, 377107000);
  assert.deepEqual(s.log.slice(4), [`bytes=${swdio.page[0]}-${swdio.page[1] - 1}`, `bytes=${swdio.blocks[0][0]}-${swdio.blocks[0][1] - 1}`]);
  await r.edges(1, 0, r.info.length);
  assert.equal(s.log.length, 6);
});

test("HTTP: a file shorter than the first read is read whole once, and not again for its block", async () => {
  const s = server(ONE, { ranges: true });
  const r = await Opened.open(wasm(), s.bytes);
  assert.equal((await r.edges(0, 0, r.info.length)).ticks.length, 15286);
  // The suffix is longer than the file: 416, then the whole file by its size.
  assert.deepEqual(s.log, [`bytes=-${FIRST_READ}`, `bytes=0-${ONE.length - 1}`]);
});

test("HTTP: a server that ignores Range sends the whole file once, and it serves every read", async () => {
  const s = server(NINE, { ranges: false });
  const r = await Opened.open(wasm(), s.bytes, 1024);
  const all = await r.edges(0, 0, r.info.length);
  assert.equal(all.ticks.length, 15286);
  assert.deepEqual(s.log, ["bytes=-1024"]);
});

test("HTTP: a compressed range, or the wrong range, is refused", async () => {
  await assert.rejects(Opened.open(wasm(), server(NINE, { ranges: true, encoding: "gzip" }).bytes, 512), /compresses it \(gzip\)/);
  const shifted = server(NINE, { ranges: true, shift: 7 });
  const { footer } = layout(NINE);
  await assert.rejects(Opened.open(wasm(), shifted.bytes, 512), new RegExp(`asked for bytes from ${footer}, got ${footer + 7}`));
});

test("HTTP, another origin: the size from a read from the start, aborted once its headers are in, then the end and blocks by explicit ranges", async () => {
  const s = server(NINE, { ranges: true, other: true });
  const { footer, page, blocks } = layout(NINE, 0);
  const r = await Opened.open(wasm(), s.bytes, 512);
  assert.deepEqual(s.log, ["bytes=0-", `bytes=${NINE.length - 512}-${NINE.length - 1}`, `bytes=${footer}-${NINE.length - 512 - 1}`]);
  assert.equal(s.aborted(), 1);
  assert.equal((await r.edges(0, 0, r.info.length)).ticks.length, 15286);
  assert.deepEqual(s.log.slice(3), [`bytes=${page[0]}-${page[1] - 1}`, `bytes=${blocks[0][0]}-${blocks[blocks.length - 1][1] - 1}`]);
});

test("HTTP, another origin: a file no longer than the first read is read once from its start, and not again for its block", async () => {
  const s = server(ONE, { ranges: true, other: true });
  const r = await Opened.open(wasm(), s.bytes);
  assert.equal((await r.edges(0, 0, r.info.length)).ticks.length, 15286);
  assert.deepEqual(s.log, ["bytes=0-"]);
  assert.equal(s.aborted(), 0);
});

test("HTTP, another origin: a server that ignores Range sends the whole file once", async () => {
  const s = server(NINE, { ranges: false, other: true });
  const r = await Opened.open(wasm(), s.bytes, 1024);
  assert.equal((await r.edges(0, 0, r.info.length)).ticks.length, 15286);
  assert.deepEqual(s.log, ["bytes=0-"]);
});

test("HTTP, another origin: an answer of the wrong length, or one with no size, is refused", async () => {
  await assert.rejects(Opened.open(wasm(), server(NINE, { ranges: true, other: true, short: true }).bytes, 512), new RegExp(`asked for bytes ${NINE.length - 512} to ${NINE.length}, got 511`));
  await assert.rejects(server(ONE, { ranges: true, other: true, short: true }).bytes.tail(FIRST_READ), new RegExp(`asked for its ${ONE.length} bytes, got ${ONE.length - 1}`));
  const unsized = server(NINE, { ranges: true, other: true, noLength: true });
  await assert.rejects(Opened.open(wasm(), unsized.bytes, 512), /read from the start says no size/);
  assert.equal(unsized.aborted(), 1);
});

test("HTTP, another origin: an empty file is read as empty", async () => {
  assert.deepEqual(await server(new Uint8Array(0), { ranges: true, other: true }).bytes.tail(512), { bytes: new Uint8Array(0), size: 0 });
});

/** Bytes whose reads wait until released, and honour their signal meanwhile. */
class SlowBytes implements Bytes {
  readonly name = "slow";
  reads = 0;
  aborts = 0;
  /** Set, every read comes at once. */
  free = false;
  #inner: Bytes;
  #release: (() => void)[] = [];
  constructor(inner: Bytes) {
    this.#inner = inner;
  }
  /** Lets every read waiting go, and every read after it. */
  release() {
    this.free = true;
    for (const r of this.#release.splice(0)) r();
  }
  /** The end comes at once: only the page and block reads wait. */
  tail(n: number) {
    return this.#inner.tail(n);
  }
  async read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array> {
    this.reads++;
    if (this.free) return this.#inner.read(from, to);
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

test("two channels asked for at once read together: their pages in one request, their blocks in another", async () => {
  const bytes = new MemoryBytes(NINE, "identify-swd.4k.tickwave");
  const r = await Opened.open(wasm(), bytes, 2048);
  const opened = bytes.reads.length;
  const [swclk, swdio] = [layout(NINE, 0), layout(NINE, 1)];
  // The two pages lie side by side before the footer; SWDIO's one block lies 41 bytes after
  // SWCLK's last.
  assert.equal(swclk.page[1], swdio.page[0]);
  assert.equal(swdio.blocks[0][0] - swclk.blocks[swclk.blocks.length - 1][1], 41);
  const [a, b] = await Promise.all([r.edges(0, 0, r.info.length), r.edges(1, 0, r.info.length)]);
  assert.deepEqual([a.ticks.length, b.ticks.length], [15286, 1874]);
  assert.deepEqual(bytes.reads.slice(opened), [
    [swclk.page[0], swdio.page[1]],
    [swclk.blocks[0][0], swdio.blocks[0][1]],
  ]);
});

test("a query's reads wait on no timer: the reads asked for in a task start when it ends", async (t) => {
  const bytes = new MemoryBytes(NINE, "identify-swd.4k.tickwave");
  const r = await Opened.open(wasm(), bytes, 2048);
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval", "setImmediate"] });
  const task = () =>
    new Promise<void>((resolve) => {
      const { port1, port2 } = new MessageChannel();
      port1.onmessage = () => {
        port1.close();
        resolve();
      };
      port2.postMessage(null);
    });
  // A thousand tasks, none of them a timer's, and the query has its answer.
  const later = async () => {
    for (let i = 0; i < 1000; i++) await task();
    return "a thousand tasks later, no answer";
  };
  const got = await Promise.race([r.edges(0, 0, r.info.length).then((e) => e.ticks.length), later()]);
  assert.equal(got, 15286);
});

test("an abort stops the request at once, and the read no other request waits on", async () => {
  const slow = new SlowBytes(new MemoryBytes(NINE, "identify-swd.4k.tickwave"));
  // The footer comes at once, with the whole file in the first read; every page and block read
  // waits. Two requests for SWCLK share its page's read.
  const s = await Opened.open(wasm(), slow);
  /** Until `f` holds: a request reaches its read after a few steps of its own. */
  const until = async (f: () => boolean) => {
    while (!f()) await new Promise((r) => setTimeout(r, 0));
  };
  const c = new AbortController();
  const first = s.edges(0, 0, s.info.length, c.signal);
  const second = s.edges(0, 0, s.info.length).catch((e: unknown) => e);
  await until(() => slow.reads === 1);
  c.abort();
  await assert.rejects(first, (e) => isAbort(e));
  // The second request still waits on the read, so it was not aborted.
  assert.equal(slow.aborts, 0);
  slow.release();
  const got = await second;
  assert.ok(!(got instanceof Error) && !isAbort(got), `the second request failed: ${got}`);
  assert.equal((got as { ticks: Float64Array }).ticks.length, 15286);
  // Alone on a read, an abort stops it.
  slow.free = false;
  setCacheBytes(1);
  const d = new AbortController();
  const reads = slow.reads;
  const third = s.edges(0, 0, s.info.length, d.signal);
  await until(() => slow.reads > reads);
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

test("a bundle asked for twice at once: the first caller's abort leaves the other's read going", async () => {
  const file = readFileSync(at("fixtures/energy.tickwave"));
  const slow = new SlowBytes(new MemoryBytes(file, "energy.tickwave"));
  const r = await Opened.open(wasm(), slow);
  const a = new AbortController();
  const first = r.bundle(a.signal);
  const second = r.bundle();
  while (slow.reads === 0) await new Promise((r) => setTimeout(r, 0));
  a.abort();
  await assert.rejects(first, { name: "AbortError" });
  slow.release();
  assert.deepEqual(Object.keys((await second) ?? {}).sort(), ["energy.annotations.toml", "energy.verge.jsonl"]);
  assert.deepEqual([slow.reads, slow.aborts], [1, 0]);
});

test("the cache: an evicted page or block is read again", async () => {
  const bytes = new MemoryBytes(NINE, "identify-swd.4k.tickwave");
  // A first read shorter than the footer, which then takes two reads.
  const r = await Opened.open(wasm(), bytes, 512);
  setCacheBytes(1);
  await r.edges(0, 0, r.info.length);
  await r.edges(0, 0, r.info.length);
  setCacheBytes(192 << 20);
  await r.edges(0, 0, r.info.length);
  await r.edges(0, 0, r.info.length);
  // Two footer reads; SWCLK's page and blocks twice while the cache held nothing, and once after
  // it held everything.
  assert.equal(bytes.reads.length, 2 + 2 * 2 + 2);
  // Each recording's open asks for the page's size again: the same size keeps every entry, and
  // a smaller one that still holds them keeps them too.
  setCacheBytes(192 << 20);
  setCacheBytes(64 << 20);
  await r.edges(0, 0, r.info.length);
  assert.equal(bytes.reads.length, 2 + 2 * 2 + 2);
  setCacheBytes(192 << 20);
});

test("a sidecar of a later minor version resolves as this one, with a note of what was left out", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const toml = readFileSync(at("fixtures/identify-swd.annotations.toml"), "utf8");
  const jsonl = execFileSync(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave"], { encoding: "utf8" });
  const resolve = (toml: string) =>
    JSON.parse(r.resolveAnnotations(handJson(toml, "hand.toml").json, ["identify-swd.swd.jsonl"], [jsonl], "identify-swd.tickwave")) as { annotations: unknown[]; notes: string[] };
  const known = resolve(toml);
  const later = resolve(toml.replace('format = "tickwave-annotations/1"', 'format = "tickwave-annotations/1.1"') + '\n[[decode]]\nid = "swd"\n');
  assert.deepEqual(later.annotations, known.annotations);
  assert.deepEqual(known.notes, []);
  assert.deepEqual(later.notes, ['the TOML file: "tickwave-annotations/1.1" is newer than this reader, which leaves out decode']);
});

test("the sidecar resolves through the Rust core, and its problems come back", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const hand = handJson(readFileSync(at("fixtures/identify-swd.annotations.toml"), "utf8"), "identify-swd.annotations.toml").json;
  const jsonl = execFileSync(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave"], { encoding: "utf8" });
  const list = (JSON.parse(r.resolveAnnotations(hand, ["identify-swd.swd.jsonl"], [jsonl], "identify-swd.tickwave")) as { annotations: { id: string; start: number; end: number | null }[] }).annotations;
  // 687 generated by the decoder, 8 hand-written.
  assert.equal(list.length, 695);
  const read = list.find((a) => a.id === "dpidr-read")!;
  assert.deepEqual([read.start, read.end], [377094557, 377140560]);
  assert.ok(list.some((a) => a.id === "swd/wait/379065159"));
  assert.throws(() => r.resolveAnnotations(hand, ["identify-swd.swd.jsonl"], [jsonl], "other.tickwave"), /names "identify-swd.tickwave", not "other.tickwave"/);
  const stale = jsonl.replace(/blake3:[0-9a-f]+/, `blake3:${"00".repeat(32)}`);
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
  assert.throws(resolve("376719436.0"), /a whole tick count or a decimal with a unit/);
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
  const list = (JSON.parse(resolve('{ clock = "1.0 MHz", reads = "3 reads" }')()) as { annotations: { id: string; values: Record<string, string> | null }[] }).annotations;
  assert.deepEqual(list.find((a) => a.id === "attach")!.values, { clock: "1.0 MHz", reads: "3 reads" });
  assert.equal(list.find((a) => a.id === "first-wait")!.values, null);
  assert.throws(resolve('{ clock = "1.0MHz" }'), /clock = "1.0MHz" is not a decimal with one space and a unit where it has one/);
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

/** An analog channel from samples, for the TypeScript overview at `levels`. */
function analogSource(r: Opened, channels: { name: string; first: number; values: Float32Array }[], levels: number[]) {
  return new MemorySource(
    {
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
    },
    { levels },
  );
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

test("analog: samples come with each whole run's smallest and largest value, from the first", async () => {
  const r = await openTickwave("fixtures/swd50-cut.tickwave");
  // The whole channel, and a window that starts off a run's boundary and ends inside a run.
  for (const [ch, s, e] of [
    [0, 0, 50000],
    [1, 1020, 4013],
  ]) {
    const got = await r.samples(ch, s, e);
    const n = Math.floor(got.values.length / 64);
    assert.ok(got.runs && n > 10);
    const want = { min: [] as number[], max: [] as number[] };
    for (let k = 0; k < n; k++) {
      const run = numbers(got.values.subarray(64 * k, 64 * k + 64));
      want.min.push(Math.min(...run));
      want.max.push(Math.max(...run));
    }
    assert.deepEqual({ min: numbers(got.runs.min), max: numbers(got.runs.max) }, want, `[${s}, ${e}), channel ${ch}`);
    // Not vacuous: the runs differ.
    assert.ok(new Set(want.min).size > 1 && new Set(want.max).size > 1);
  }
});

test("analog: each channel's overview, its blocks' and its strip's, as the TypeScript computation has it from the samples", async () => {
  const r = await openTickwave("fixtures/swd50-cut.tickwave");
  // Samples a tick apart: round(log2(64)) = 6, then steps of 4 to a strip of 674 buckets.
  const file = readFileSync(at("fixtures/swd50-cut.tickwave"));
  for (const ch of [0, 1]) assert.deepEqual(formatLevels(r.info.length, finest(file, ch)), [64, 1024, 16384]);
  const values = await Promise.all([0, 1].map((ch) => r.samples(ch, 0, r.info.length)));
  const m = analogSource(r, [
    { name: "SWCLK", first: 0, values: values[0].values as Float32Array },
    { name: "SWDIO", first: 0, values: values[1].values as Float32Array },
  ], [64, 1024, 16384]);
  for (const bucket of [64, 1024, 16384]) {
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

test("analog overview blocks: session50's 2^6 and 2^10 levels, as the TypeScript computation has them", { skip: !existsSync(SESSION50) && "no fixtures-local/session50.tickwave" }, async () => {
  const r = await openTickwave(SESSION50.pathname);
  // Samples a tick apart: a finest level of 2^6, then 2^10, each in overview blocks.
  for (const ch of [0, 1]) assert.deepEqual(r.header.channels[ch].overview_blocks?.slice(0, 2).map((l) => l.shift), [6, 10]);
  // The analog samples end at tick 100,002,000, inside the 2^10 level's overview block 23, though
  // the capture runs to 1.26 s. Two windows of whole buckets: one inside the samples, one across
  // their end.
  for (const [s, e] of [
    [49 * 2 ** 20, 50 * 2 ** 20],
    [95 * 2 ** 20, 96 * 2 ** 20],
  ]) {
    for (const ch of [0, 1]) {
      const c = r.info.channels[ch];
      assert.ok(c.kind === "analog" && c.period.num === 1 && c.period.den === 1 && c.first === 0);
      assert.equal(c.end, 100002000);
      const values = await r.samples(ch, s, e);
      const m = analogSource(r, [{ name: c.name, first: values.first, values: values.values as Float32Array }], [64, 1024]);
      for (const bucket of [64, 1024]) {
        const got = (await r.overview(ch, s, e, bucket))!;
        const want = (await m.overview(0, s, e, bucket))!;
        assert.ok(got.kind === "analog" && want.kind === "analog");
        assert.equal(got.bucket, bucket);
        // Not vacuous: the buckets hold samples.
        assert.ok(got.min.length > 0 && numbers(got.min).every(Number.isFinite), `[${s}, ${e}) has samples`);
        assert.deepEqual([got.start, numbers(got.min), numbers(got.max)], [want.start, numbers(want.min), numbers(want.max)], `[${s}, ${e}), channel ${ch}, ${bucket}`);
      }
    }
  }
});

test("duration: first edge to last, of either polarity, of the edges in [start, end)", async () => {
  const r = await openTickwave("fixtures/identify-swd.tickwave");
  const tb = r.info.timebase;
  const duration = async (ch: number, s: number, e: number) => {
    const edges = await r.edges(ch, s, e);
    return { edges, reading: measureReading("duration", edges.level, edges.ticks, tb, STEP_ONE) };
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
});

/** Everything a view can read of a file: its footer, then each channel's every level over the whole
 * capture, a bucket of each power of two asking for the coarsest no wider, and its items. */
async function readAll(file: Uint8Array, name: string): Promise<void> {
  const r = await Opened.open(wasm(), new MemoryBytes(file, name));
  for (const [c, ch] of r.info.channels.entries()) {
    for (let s = 62; s >= 0; s--) await r.overview(c, 0, r.info.length, 2 ** s);
    await r.overview(c, 0, r.info.length, Infinity);
    if (ch.kind === "digital") await r.edges(c, 0, r.info.length);
    else await r.samples(c, 0, r.info.length);
  }
}

test("a crafted recording is refused with an error, and the module goes on decoding", async () => {
  const why: Record<string, RegExp> = {
    "header-wrap": /footer's lengths do not fit it/,
    levels: /not the ones the format sets/,
    "big-decoded": /past the format's limits/,
    "huge-count": /more than the 4194304 a block holds/,
    "page-count": /its head differs from where the table's shape puts it/,
  };
  for (const [name, expected] of Object.entries(why)) {
    await assert.rejects(
      readAll(readFileSync(at(`fixtures/crafted/${name}.tickwave`)), name),
      (e: unknown) => !(e instanceof WebAssembly.RuntimeError) && expected.test(String((e as Error).message)),
      name,
    );
    // A panic would have ended the module; a refusal leaves it decoding the next file.
    await readAll(ONE, "identify-swd.tickwave");
  }
  // Asked of what the recording doesn't hold, the decoder throws rather than reading out of
  // bounds.
  const r = new (wasm().Recording)(ONE, ONE.length);
  const page = [0, 3727, 60, 0, 0, NaN, NaN];
  for (const [what, f] of Object.entries({
    "a channel past the last": () => r.blockPages(2, 0, 1),
    "a level it lacks": () => r.levelPages(0, 0, 0, 1),
    "a list it lacks": () => r.listPages(0, 0, 1),
    "a located page of six numbers": () => r.blockPage(0, page.slice(0, 6), ONE.subarray(3727, 3787)),
    "a page's bytes cut short": () => r.blockPage(0, page, ONE.subarray(3727, 3786)),
    "a block of seven numbers": () => r.decodeBlockAt(0, [0, 1, 0, 527, 1727, 4000, 0], ONE.subarray(527, 2254)),
    "a block's frame cut short": () => r.decodeBlockAt(0, [0, 523583100, 0, 527, 1727, 4000, 0, NaN], ONE.subarray(527, 2253)),
    "a list of five numbers": () => r.decodeBlockAt(0, [0, 523583100, 0, 527, 1727, 4000, 0, NaN], ONE.subarray(527, 2254), new Float64Array(5)),
    "a derived channel it lacks": () => r.productValues(0, new Float32Array(1), new Float32Array(1)),
    "an overview row of four numbers": () => r.decodeOverviewAt(0, 0, [0, 0, 0, 0], new Uint8Array(1)),
    "a bundle it lacks": () => r.bundleFiles(new Uint8Array(4)),
  })) {
    assert.throws(f, (e: unknown) => !(e instanceof WebAssembly.RuntimeError) && e instanceof Error, what);
  }
  // The page handed in as it lies, which those refusals left the module to read.
  assert.equal(r.blockPage(0, page, ONE.subarray(3727, 3787)).length, 8);
});

test("the most the decoder takes from one frame is the page's to set", async () => {
  // identify-jtag's strips, of 598 buckets of three digital channels, decode to more than 64 bytes
  // each; the first draw reads them.
  const jtag = readFileSync(at("fixtures/identify-jtag.tickwave"));
  const small = await Opened.open(wasm(), new MemoryBytes(jtag, "jtag.tickwave"), FIRST_READ, 64);
  await assert.rejects(small.overview(0, 0, small.info.length, Infinity), /more than this reader holds \(64\)/);
  const r = await Opened.open(wasm(), new MemoryBytes(jtag, "jtag.tickwave"), FIRST_READ, 1 << 20);
  assert.ok((await r.overview(0, 0, r.info.length, Infinity))!.bucket === 2 ** 20);
});

test("a channel known only from a gap's end starts there, at the gap's level, and so does the rest after any gap", async () => {
  // Written by crates/format/examples/gaps.rs: A is not known until 300 µs, high there, and runs a
  // 10 µs clock from 400 µs; B is known throughout.
  const r = await openTickwave("fixtures/leading-gap.tickwave");
  const [a, b] = r.info.channels;
  assert.deepEqual([a, b], [
    { kind: "digital", name: "A", sourceName: "a", source: 0, end: 1_000_000, start: 300_000 },
    { kind: "digital", name: "B", sourceName: "b", source: 0, end: 1_000_000 },
  ]);
  const e = await r.edges(0, 300_000, 410_000);
  assert.equal(e.level, 1);
  assert.deepEqual([...e.ticks], [400_000, 405_000]);
  // A is not known from 600 to 700 µs, high after it; its clock's edges fall every 5 µs, none in
  // the gap, so the next is a fall at 705 µs.
  const m = await openTickwave("fixtures/middle-gap.tickwave");
  const ma = m.info.channels[0];
  assert.deepEqual(ma.kind === "digital" && ma.gaps, [{ from: 600_000, to: 700_000, level: 1 }]);
  for (const from of [700_000, 702_000]) {
    const after = await m.edges(0, from, 720_000);
    assert.deepEqual([after.level, [...after.ticks]], [1, [705_000, 710_000, 715_000]], `from ${from}`);
  }
  // Before the gap the clock runs as before: 40 edges from 400 µs leave it low at 600 µs.
  const before = await m.edges(0, 590_000, 600_000);
  assert.deepEqual([before.level, [...before.ticks]], [0, [590_000, 595_000]]);
});

test("a gap that ends where a block starts sets the level there, read from inside that block", async (t) => {
  // crates/format/examples/block_gap.rs: D rises at 10, is not known from 50 to 100 and low after,
  // the second block starts at 100, and D rises at 150. The block's level bit carries the level
  // from before the gap, high, so the reader takes the gap's.
  const dir = mkdtempSync(join(tmpdir(), "escapement-tickwave-block-gap-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  execFileSync("cargo", ["run", "-q", "-p", "escapement-tickwave-format", "--example", "block_gap", "--", dir], { cwd: new URL("../..", import.meta.url).pathname, stdio: ["ignore", "ignore", "inherit"] });
  const r = await openTickwave(join(dir, "block-gap.tickwave"));
  for (const [from, to] of [[100, 200], [120, 170], [0, 200]]) {
    const e = await r.edges(0, from, to);
    assert.deepEqual([e.level, [...e.ticks]], [0, from === 0 ? [10, 150] : [150]], `edges ${from}..${to}`);
  }
});
