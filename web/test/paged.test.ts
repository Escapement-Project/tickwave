// A reader that reads a page at a time, over recordings crates/format/examples/paged.rs writes: a
// block table under a directory page, and channels whose gaps or breaks are in list pages. Which
// bytes a view should fetch comes from test/layout.ts; the lists, edges and samples a view should
// get come from what the example wrote (lists.json) and the rules it wrote samples by.

import { before, after, test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { MemoryBytes } from "../src/wasm/bytes.ts";
import { handJson } from "../src/wasm/hand.ts";
import { Opened } from "../src/wasm/opened.ts";
import { channelSampleTick, endLosses, knownSpans, sampleAtOrBefore, spanned, type AnalogChannel, type Break, type Gap } from "../src/source.ts";
import { wasm } from "../scripts/node-wasm.ts";
import { blockRows, directories, layout, leaves, range, type Entry } from "./layout.ts";

const dir = mkdtempSync(join(tmpdir(), "escapement-tickwave-paged-"));
after(() => rmSync(dir, { recursive: true, force: true }));
let LISTS: Uint8Array;
let BLOCKS: Uint8Array;
let PHASED: Uint8Array;
let written: { length: number; breaks: [number, number, number][]; gaps: [number, number, boolean][]; edges: number[] };

before(() => {
  execFileSync("cargo", ["run", "-q", "-p", "escapement-tickwave-format", "--example", "paged", "--", dir], { cwd: new URL("../..", import.meta.url).pathname, stdio: ["ignore", "ignore", "inherit"] });
  LISTS = new Uint8Array(readFileSync(join(dir, "lists.tickwave")));
  BLOCKS = new Uint8Array(readFileSync(join(dir, "blocks.tickwave")));
  PHASED = new Uint8Array(readFileSync(join(dir, "phased-lists.tickwave")));
  written = JSON.parse(readFileSync(join(dir, "lists.json"), "utf8"));
});

const open = async (file: Uint8Array, name: string) => {
  const bytes = new MemoryBytes(file, name);
  return { bytes, r: await Opened.open(wasm(), bytes) };
};

/** How many of `pages` the reads touch. */
const touched = (pages: readonly Entry[], reads: readonly [number, number][]) => pages.filter((p) => reads.some(([a, b]) => a < p.offset + p.length && p.offset < b)).length;

test("a block table under a directory: the directory read once, then the page a span needs", async () => {
  const { bytes, r } = await open(BLOCKS, "blocks.tickwave");
  const t = layout(BLOCKS);
  const f = t.channels[0].blocks!;
  // 70,001 blocks: 18 pages, so a directory page lists them and the top index lists it.
  assert.equal(f.depth, 1);
  const dirs = directories(BLOCKS, f);
  const pages = leaves(BLOCKS, f);
  const rows = blockRows(BLOCKS, f);
  assert.deepEqual([dirs.length, pages.length, rows.length], [1, 18, 70_001]);
  // F's block k runs from 10 k to 10 k + 10 and holds the edge at 10 k + 5: a window of ten of
  // them on page 7, blocks 28,672 to 32,767, which lie one after another with a known-through
  // record between each two: one read.
  const e = await r.edges(0, 300_000, 300_100);
  assert.deepEqual([e.level, [...e.ticks]], [0, Array.from({ length: 10 }, (_, i) => 300_005 + 10 * i)]);
  assert.deepEqual(bytes.reads.slice(1), [range(dirs[0]), range(pages[7]), [rows[30_000].from, rows[30_009].to]]);
  // Elsewhere: the page that holds it and its block, the directory not read again.
  const at = bytes.reads.length;
  const later = await r.edges(0, 650_000, 650_010);
  assert.deepEqual([...later.ticks], [650_005]);
  assert.deepEqual(bytes.reads.slice(at), [range(pages[15]), [rows[65_000].from, rows[65_000].to]]);
  // G's own page and its three blocks, none of F's.
  const g = t.channels[1].blocks!;
  const gRows = blockRows(BLOCKS, g);
  const before = bytes.reads.length;
  assert.deepEqual([...(await r.edges(1, 0, r.info.length)).ticks], [100, 600_000]);
  assert.deepEqual(bytes.reads.slice(before), [range(g.entries[0]), ...gRows.map((b) => [b.from, b.to])]);
});

test("a bundled sidecar that doesn't resolve says why when asked for, and the recording reads", async () => {
  const { bytes, r } = await open(BLOCKS, "blocks.tickwave");
  // The footer says there is a bundle; nothing of it is read until asked for.
  assert.equal(r.info.bundled, true);
  assert.deepEqual(bytes.reads, [[BLOCKS.length - 32 * 1024, BLOCKS.length]]);
  const files = (await r.bundle())!;
  assert.deepEqual(bytes.reads.slice(1), [layout(BLOCKS).bundle]);
  assert.deepEqual(Object.keys(files), ["blocks.annotations.toml"]);
  // Its hand-written file names another recording, which the resolve refuses with the reason.
  const hand = handJson(files["blocks.annotations.toml"], "blocks.annotations.toml").json;
  assert.throws(() => r.resolveAnnotations(hand, [], [], null), /blake3:0000/);
  assert.deepEqual([...(await r.edges(1, 0, r.info.length)).ticks], [100, 600_000]);
});

/** Sample n's tick, by the example's runs: run k of 20 from 100 + 250 k, a sample every 10. */
const tick = (n: number) => 100 + 250 * Math.floor(n / 20) + 10 * (n % 20);
const COUNT = 200_020;

/** The samples whose ticks lie in [s, e): their first index and how many. */
function samplesIn(s: number, e: number): [number, number] {
  let first = -1;
  let n = 0;
  for (let i = 0; i < COUNT; i++) {
    if (tick(i) >= s && tick(i) < e) {
      if (first < 0) first = i;
      n++;
    }
  }
  return [first, n];
}

/** Whether `got` is a run of `all` in order, holding every entry `need` keeps. */
function runOf<T>(got: readonly T[], all: readonly T[], need: (x: T, i: number) => boolean): boolean {
  const from = all.findIndex((x) => JSON.stringify(x) === JSON.stringify(got[0]));
  if (from < 0 || got.some((x, i) => JSON.stringify(x) !== JSON.stringify(all[from + i]))) return false;
  return all.every((x, i) => !need(x, i) || (i >= from && i < from + got.length));
}

test("a channel's breaks in pages: a window reads the pages it needs, and its samples are placed by them", async () => {
  const { bytes, r } = await open(LISTS, "lists.tickwave");
  const t = layout(LISTS);
  const listPages = leaves(LISTS, t.channels[0].list!);
  assert.equal(listPages.length, 3);
  const [a, , , p] = r.info.channels;
  assert.ok(a.kind === "analog" && p.kind === "analog");
  // Its breaks are not read at open: it runs to the length until an answer reaches its end.
  assert.deepEqual([a.paged, a.breaks, a.end, a.lostBefore, a.lostAfter], [true, undefined, written.length, 3, 2]);
  const breaks: Break[] = written.breaks.map(([index, tick, lost]) => ({ index, tick, lost }));
  // Page 1 starts at the break at tick 1,024,350, page 2 at 2,048,350.
  assert.deepEqual([listPages[1].first, listPages[2].first], [1_024_350, 2_048_350]);
  // Two that need a neighbouring page: one starting just past page 2's first break, whose
  // break before it is page 1's last; one ending just before it, whose first break at or past its
  // end is that one.
  for (const [s, e, most] of [
    [1_500_000, 1_500_600, 1],
    [1_024_000, 1_025_000, 2],
    [2_048_000, 2_048_350, 2],
    [2_048_400, 2_048_900, 2],
    [2_047_900, 2_048_300, 2],
    [0, 1000, 1],
    [2_500_000, written.length, 1],
  ]) {
    const at = bytes.reads.length;
    const got = await r.samples(0, s, e);
    const [first, n] = samplesIn(s, e);
    assert.deepEqual([got.first, got.values.length], [first, n], `[${s}, ${e})`);
    assert.deepEqual([...got.values], Array.from({ length: n }, (_, i) => 1000 + ((first + i) % 97)), `[${s}, ${e})`);
    // The breaks it needs: from the one before the last at or before s through the first at or
    // past e.
    const listed = got.listed!.breaks!;
    const last = breaks.findLastIndex((b) => b.tick <= s);
    const next = breaks.findIndex((b) => b.tick >= e);
    assert.ok(runOf(listed, breaks, (b, i) => (b.tick >= s && b.tick < e) || i === last || i === last - 1 || i === next), `[${s}, ${e}): ${listed.length} breaks`);
    assert.ok(touched(listPages, bytes.reads.slice(at)) <= most, `[${s}, ${e}) read ${touched(listPages, bytes.reads.slice(at))} list pages`);
    // P is A times B, sample by sample, from the same breaks.
    const power = await r.samples(3, s, e);
    assert.deepEqual([...power.values], Array.from({ length: n }, (_, i) => (1000 + ((first + i) % 97)) * (2000 + ((31 * (first + i)) % 89))), `P [${s}, ${e})`);
  }
  // The window at the end reaches the last run: the channel ends where its last sample's time
  // does, 2,500,300, and the two samples lost after it follow, as do the three lost before tick
  // 100 on the first run's grid.
  const end = (await r.samples(0, 2_500_000, written.length)).listed!;
  assert.equal(end.end, 2_500_300);
  assert.deepEqual(endLosses(spanned(a, end) as AnalogChannel), [
    { from: 70, to: 100, lost: 3 },
    { from: 2_500_300, to: 2_500_320, lost: 2 },
  ]);
  // Away from the end, the place after the last sample is not known, nor the loss after it.
  const middle = (await r.samples(0, 1_500_000, 1_500_600)).listed!;
  assert.equal(middle.end, undefined);
  assert.deepEqual(endLosses(spanned(a, middle) as AnalogChannel), [{ from: 70, to: 100, lost: 3 }]);
  // The runs the window holds, from its breaks: runs 5,999 to 6,001, from 1,499,850, 1,500,100 and
  // 1,500,350, each 200 ticks long.
  assert.deepEqual(knownSpans(spanned(a, middle), 1_500_000, 1_500_600), [
    [1_500_000, 1_500_050],
    [1_500_100, 1_500_300],
    [1_500_350, 1_500_550],
  ]);
});

test("a channel's gaps in pages: edges read the pages a window needs, and the level after a gap is the gap's", async () => {
  const { bytes, r } = await open(LISTS, "lists.tickwave");
  const t = layout(LISTS);
  const listPages = leaves(LISTS, t.channels[2].list!);
  assert.equal(listPages.length, 3);
  const d = r.info.channels[2];
  assert.deepEqual([d.kind, d.kind === "digital" && d.paged, d.kind === "digital" && d.gaps, d.kind === "digital" && d.start], ["digital", true, undefined, undefined]);
  const gaps: Gap[] = written.gaps.map(([from, to, level]) => ({ from, to, level: level ? 1 : 0 }));
  /** The level at `s`, out of a gap: low until 30, each edge flips it, a gap's end sets it. */
  const level = (s: number) => {
    let at = 0;
    const events = [...written.edges.map((t) => ({ t, gap: null as Gap | null })), ...gaps.map((g) => ({ t: g.to, gap: g }))].sort((x, y) => x.t - y.t);
    for (const ev of events) {
      if (ev.t > s || (ev.t === s && !ev.gap)) break;
      at = ev.gap ? ev.gap.level : at ^ 1;
    }
    return at;
  };
  // Page 1 starts at the gap from 1,024,000, page 2 at 2,048,000. Windows that start out of a gap.
  // D's three blocks span about a million ticks each, and a block is checked against the gaps its
  // ticks need, so a window reads the pages its block spans: two, but for the first block's.
  // And one starting just past page 2's first gap, whose gap before it is page 1's last.
  for (const [s, e, most] of [
    [1_500_120, 1_500_700, 2],
    [1_023_900, 1_024_400, 2],
    [2_047_920, 2_048_000, 2],
    [2_048_060, 2_048_200, 2],
    [40, 900, 1],
    [2_249_980, written.length, 2],
  ]) {
    const at = bytes.reads.length;
    const got = await r.edges(2, s, e);
    assert.deepEqual([...got.ticks], written.edges.filter((t) => t >= s && t < e), `[${s}, ${e})`);
    assert.equal(got.level, level(s), `the level at ${s}`);
    // The gaps it needs: the last two from at or before s, every one from inside, the first after.
    const listed = got.listed!;
    const lastTwo = gaps.findLastIndex((g) => g.from <= s);
    const next = gaps.findIndex((g) => g.from >= e);
    // The gap from the channel's start is not a gap in its data: it says where the level is
    // first known.
    const want = (g: Gap, i: number) => i > 0 && ((g.from >= s && g.from < e) || i === lastTwo || i === lastTwo - 1 || i === next);
    assert.ok(runOf(listed.gaps!, gaps, want), `[${s}, ${e}): ${listed.gaps!.length} gaps`);
    assert.equal(listed.start, s === 40 ? 30 : undefined);
    assert.ok(touched(listPages, bytes.reads.slice(at)) <= most, `[${s}, ${e}) read ${touched(listPages, bytes.reads.slice(at))} list pages`);
  }
});

test("breaks in pages carry their phases: a window's samples sit at the ticks their places map to", async () => {
  // C: run k of 20 samples from tick 100 + 100 k, the first at the declared phase of 3/4 of a
  // tick, run k after it k % 4 quarter ticks past its tick; every 5/2 ticks. Sample n holds code n.
  const { r } = await open(PHASED, "phased-lists.tickwave");
  const c = r.info.channels[0];
  assert.ok(c.kind === "analog" && c.paged);
  const tickOf = (n: number) => {
    const [k, j] = [Math.floor(n / 20), n % 20];
    return Math.floor(100 + 100 * k + (k === 0 ? 3 : k % 4) / 4 + 2.5 * j);
  };
  let checked = 0;
  // Windows over runs 1 to 3, over the end of run 100 and the start of 101 across the pause
  // between, and over run 197: each holds a run whose phase is not 0.
  for (const [s, e] of [
    [200, 500],
    [10_140, 10_260],
    [19_800, 19_900],
  ]) {
    const got = await r.samples(0, s, e);
    const place = spanned(c, got.listed) as AnalogChannel;
    assert.ok(got.listed!.breaks!.some((b) => (b.phase ?? 0) !== 0), `[${s}, ${e})`);
    for (let i = 0; i < got.values.length; i++) {
      const n = got.first + i;
      assert.equal(got.values[i], n, `[${s}, ${e}) sample ${n}`);
      assert.equal(channelSampleTick(place, n), tickOf(n), `[${s}, ${e}) sample ${n}`);
      assert.equal(sampleAtOrBefore(place, tickOf(n)), n, `[${s}, ${e}) sample ${n}`);
      checked++;
    }
    // The window holds every sample whose tick lies in it, and no other.
    const want = Array.from({ length: 4000 }, (_, n) => n).filter((n) => tickOf(n) >= s && tickOf(n) < e);
    assert.deepEqual([got.first, got.values.length], [want[0], want.length], `[${s}, ${e})`);
  }
  assert.ok(checked > 50, `${checked}`);
});

test("a first draw reads no list page; the exact answer reads those its span needs", async () => {
  const { bytes, r } = await open(LISTS, "lists.tickwave");
  const t = layout(LISTS);
  assert.deepEqual(bytes.reads, [[LISTS.length - 32 * 1024, LISTS.length]]);
  for (const [c, ch] of r.info.channels.entries()) {
    const strip = (await r.overview(c, 0, ch.end, Infinity))!;
    assert.equal(strip.bucket, 2 ** 14);
    // Not assert.equal: its message would print every entry of a list, and diffing them runs
    // out of memory.
    assert.ok(strip.listed === undefined, `channel ${c}'s strip carries a list`);
  }
  assert.equal(bytes.reads.length, 1);
  // The whole capture at the strip's level, exactly: every list page of A and of D, and P takes
  // A's.
  for (const c of [0, 2, 3]) {
    const exact = (await r.overview(c, 0, r.info.length, 2 ** 14))!;
    assert.ok(exact.listed && (exact.listed.breaks ?? exact.listed.gaps)!.length > 8000, `channel ${c}'s list`);
  }
  const pages = [0, 2].flatMap((c) => leaves(LISTS, t.channels[c].list!));
  assert.equal(touched(pages, bytes.reads.slice(1)), 6);
  assert.equal(touched(leaves(LISTS, t.channels[1].list!), bytes.reads.slice(1)), 0);
});
