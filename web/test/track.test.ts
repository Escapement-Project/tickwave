// What a row draws while its next tile is on the way: a view that outruns its fetch keeps every
// column drawn, from the tile it has and the coarse base, rather than going blank.

import { test } from "node:test";
import assert from "node:assert/strict";
import { digitalFromEdges, digitalFromLevels, NONE } from "../src/render/columns.ts";
import { EDGES_AT_MOST, Track } from "../src/render/track.ts";
import { MemorySource } from "../src/memory-source.ts";

// A clock edge every 1,000 ticks over 2^24 ticks: overview levels of 4,096 and 65,536 ticks.
const LENGTH = 2 ** 24;
const COLUMNS = 1000;

test("a track asks for its coarsest level when it is made, before any draw asks for a view", () => {
  const asked: number[] = [];
  const source = new MemorySource({ title: "c", timebase: { num: 1e9, den: 1 }, length: LENGTH, channels: [{ kind: "digital", name: "CLK", end: LENGTH, initial: 0, ticks: new Float64Array([10]) }] });
  const overview = source.overview.bind(source);
  source.overview = (c, s, e, maxBucket, signal) => {
    asked.push(maxBucket);
    return overview(c, s, e, maxBucket, signal);
  };
  new Track(source, 0, { kind: "digital", name: "CLK", end: LENGTH }, () => {}, () => {});
  assert.deepEqual(asked, [Infinity]);
});

function clockTrack(): Track {
  const ticks = new Float64Array(Math.floor(LENGTH / 1000) - 1).map((_, i) => (i + 1) * 1000);
  const source = new MemorySource({
    title: "clock",
    timebase: { num: 1e9, den: 1 },
    length: LENGTH,
    channels: [{ kind: "digital", name: "CLK", end: LENGTH, initial: 0, ticks }],
  });
  return new Track(source, 0, { kind: "digital", name: "CLK", end: LENGTH }, () => {}, (e) => assert.fail(String(e)));
}

/** Lets every answer the source owes arrive. */
const arrive = () => new Promise((r) => setTimeout(r, 0));

/** How many of the view's columns no layer draws. */
function blank(track: Track, start: number, end: number): number {
  const g = { x0: start, per: (end - start) / COLUMNS, count: COLUMNS };
  const drawn = new Uint8Array(COLUMNS);
  for (const { tile, start: from, end: to } of track.layers()) {
    if (tile.kind !== "edges" && tile.kind !== "digital-overview") assert.fail(tile.kind);
    const cols = tile.kind === "edges" ? digitalFromEdges(g, tile.data, from, to) : digitalFromLevels(g, tile.data, from, to);
    cols.state.forEach((s, c) => {
      if (s !== NONE) drawn[c] = 1;
    });
  }
  return drawn.filter((d) => d === 0).length;
}

test("zooming in draws the coarser tile until the finer one arrives", async () => {
  const track = clockTrack();
  track.need(0, LENGTH, LENGTH / COLUMNS);
  await arrive();
  assert.equal(track.tile?.kind, "digital-overview");
  // A hundredth of the capture: 168 ticks a column, finer than any overview level.
  const [start, end] = [8_000_000, 8_000_000 + LENGTH / 100];
  track.need(start, end, (end - start) / COLUMNS);
  assert.ok(track.busy);
  assert.equal(blank(track, start, end), 0);
  await arrive();
  assert.equal(track.tile?.kind, "edges");
  assert.equal(blank(track, start, end), 0);
});

test("zooming out draws the base where the tile does not reach", async () => {
  const track = clockTrack();
  const [start, end] = [8_000_000, 8_000_000 + LENGTH / 100];
  track.need(start, end, (end - start) / COLUMNS);
  await arrive();
  assert.equal(track.tile?.kind, "edges");
  // The edges cover three hundredths of the capture. The base is the coarsest level, asked for
  // on the first need.
  track.need(0, LENGTH, LENGTH / COLUMNS);
  assert.ok(track.busy);
  assert.equal(blank(track, 0, LENGTH), 0);
  const base = track.layers().find((l) => l.tile !== track.tile);
  assert.ok(base && base.tile.kind === "digital-overview");
  assert.equal(base.tile.data.bucket, 65536);
  await arrive();
  // The full view's tile covers the channel, and replaces the coarser base.
  assert.equal(track.tile?.kind, "digital-overview");
  assert.deepEqual(
    track.layers().map((l) => l.tile),
    [track.tile],
  );
});

// A burst far denser than the channel's average: EDGES_AT_MOST + 100,000 edges two ticks apart
// from 2^29, in 2^30 ticks whose levels are 2^24 and 2^28 ticks, so the whole view's columns are
// already finer than the finest level.
const BURST = { length: 2 ** 30, at: 2 ** 29, edges: EDGES_AT_MOST + 100_000 };

function burstTrack(): { track: Track; asked: { edges: number; overviews: number[] } } {
  const ticks = new Float64Array(BURST.edges).map((_, i) => BURST.at + 2 * i);
  const source = new MemorySource(
    { title: "burst", timebase: { num: 1e9, den: 1 }, length: BURST.length, channels: [{ kind: "digital", name: "D", end: BURST.length, initial: 0, ticks }] },
    { levels: [2 ** 24, 2 ** 28] },
  );
  const asked = { edges: 0, overviews: [] as number[] };
  const [edges, overview] = [source.edges.bind(source), source.overview.bind(source)];
  source.edges = (...a) => {
    asked.edges++;
    return edges(...a);
  };
  source.overview = (c, s, e, maxBucket, signal) => {
    asked.overviews.push(maxBucket);
    return overview(c, s, e, maxBucket, signal);
  };
  const track = new Track(source, 0, { kind: "digital", name: "D", end: BURST.length }, () => {}, (e) => assert.fail(String(e)));
  return { track, asked };
}

/** Lets the track's read under way, and every read it leads to, land. */
async function landed(track: Track): Promise<void> {
  for (let i = 0; i < 100 && track.busy; i++) await arrive();
  assert.ok(!track.busy);
}

test("a row whose read would hold more edges than it reads at most draws its finest overview level, and reads none", async () => {
  const { track, asked } = burstTrack();
  await arrive();
  track.need(0, BURST.length, BURST.length / COLUMNS);
  await landed(track);
  assert.equal(asked.edges, 0);
  assert.equal(track.tile?.kind, "digital-overview");
  assert.equal(track.tile.data.bucket, 2 ** 24);
  assert.ok(track.settled(0, BURST.length, BURST.length / COLUMNS));
  // The view holds every column: the burst's buckets across the columns they span.
  assert.equal(blank(track, 0, BURST.length), 0);
  // A view less than twice as fine, whose read lies inside that one, asks for nothing more.
  const asks = asked.overviews.length;
  const [start, end] = [0.2 * BURST.length, 0.8 * BURST.length];
  track.need(start, end, (end - start) / COLUMNS);
  assert.ok(!track.busy);
  assert.equal(asked.overviews.length, asks);
});

test("zoomed in until its read holds few enough edges, the row reads them", async () => {
  const { track, asked } = burstTrack();
  await arrive();
  track.need(0, BURST.length, BURST.length / COLUMNS);
  await landed(track);
  // A view of 2^20 ticks in the burst reads three times that: 1,572,864 edges.
  const [start, end] = [BURST.at + 2 ** 20, BURST.at + 2 ** 21];
  track.need(start, end, (end - start) / COLUMNS);
  await landed(track);
  assert.equal(asked.edges, 1);
  assert.equal(track.tile?.kind, "edges");
  assert.equal(track.tile.data.ticks.length, (3 * 2 ** 20) / 2);
});

test("a row moved forgets where its edges were too many, and counts them again", async () => {
  const { track } = burstTrack();
  await arrive();
  track.need(0, BURST.length, BURST.length / COLUMNS);
  await landed(track);
  assert.equal(track.tile?.kind, "digital-overview");
  // Moved a little, the tile moves with it and still covers a view inside what was read: the row
  // reads again rather than taking the tile for what that view wants.
  track.moveBy({ kind: "digital", name: "D", end: BURST.length }, 1000);
  const [start, end] = [0.2 * BURST.length, 0.8 * BURST.length];
  track.need(start, end, (end - start) / COLUMNS);
  assert.ok(track.busy);
});

test("a row whose overview in hand holds too few edges to pass the bound reads its edges without counting them first", async () => {
  const ticks = new Float64Array(Math.floor(LENGTH / 1000) - 1).map((_, i) => (i + 1) * 1000);
  const source = new MemorySource({ title: "clock", timebase: { num: 1e9, den: 1 }, length: LENGTH, channels: [{ kind: "digital", name: "CLK", end: LENGTH, initial: 0, ticks }] });
  let counted = 0;
  const count = source.edgeCount.bind(source);
  source.edgeCount = (...a) => {
    counted++;
    return count(...a);
  };
  const track = new Track(source, 0, { kind: "digital", name: "CLK", end: LENGTH }, () => {}, (e) => assert.fail(String(e)));
  await arrive();
  // A hundredth of the capture, finer than any level: its read holds about 500 edges.
  const [start, end] = [8_000_000, 8_000_000 + LENGTH / 100];
  track.need(start, end, (end - start) / COLUMNS);
  await landed(track);
  assert.equal(track.tile?.kind, "edges");
  assert.equal(counted, 0);
});

test("an analog row's whole range is its coarsest tile's least and greatest, and none before that tile lands", async () => {
  const values = Float64Array.from({ length: 5_000 }, (_, i) => Math.sin(i / 37) * 3);
  [values[77], values[100], values[4_321]] = [-7, NaN, 12];
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 50_000, channels: [{ kind: "analog", name: "A", end: 50_000, unit: "V", first: 0, period: { num: 10, den: 1 }, resolution: null, values }] },
    { levels: [64, 1024], latency: 5 },
  );
  const info = await source.info();
  const track = new Track(source, 0, info.channels[0], () => {}, (e) => assert.fail(String(e)));
  // The base is on its way: no range to show yet.
  assert.equal(track.wholeRange(), null);
  await new Promise((r) => setTimeout(r, 30));
  let [lo, hi] = [Infinity, -Infinity];
  for (const v of values) {
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  assert.deepEqual(track.wholeRange(), { lo, hi });
  assert.equal(lo, -7);
  assert.equal(hi, 12);
});

test("an analog row with no overview has its whole range once a read covers its channel, from its samples, leaving out any that is not a number", async () => {
  const values = Float64Array.from({ length: 5_000 }, (_, i) => Math.cos(i / 41) * 2);
  [values[0], values[3], values[2_500]] = [NaN, -5, 9];
  const source = new MemorySource(
    { title: "a", timebase: { num: 1e6, den: 1 }, length: 50_000, channels: [{ kind: "analog", name: "A", end: 50_000, unit: "V", first: 0, period: { num: 10, den: 1 }, resolution: null, values }] },
    { levels: [] },
  );
  const info = await source.info();
  const track = new Track(source, 0, info.channels[0], () => {}, (e) => assert.fail(String(e)));
  // A view of part of it reads part of it: no range.
  track.need(10_000, 12_000, 2);
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(track.tile?.kind, "samples");
  assert.equal(track.wholeRange(), null);
  // The whole of it.
  track.need(0, 50_000, 50);
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual(track.wholeRange(), { lo: -5, hi: 9 });
});
