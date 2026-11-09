// What a row draws while its next tile is on the way: a view that outruns its fetch keeps every
// column drawn, from the tile it has and the coarse base, rather than going blank.

import { test } from "node:test";
import assert from "node:assert/strict";
import { digitalFromEdges, digitalFromOverview, NONE } from "../src/render/columns.ts";
import { Track } from "../src/render/track.ts";
import { MemorySource } from "../src/memory-source.ts";

// A clock edge every 1,000 ticks over 2^24 ticks: overview levels of 4,096 and 65,536 ticks.
const LENGTH = 2 ** 24;
const COLUMNS = 1000;

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
    const cols = tile.kind === "edges" ? digitalFromEdges(g, tile.data, from, to) : digitalFromOverview(g, tile.data, from, to);
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
