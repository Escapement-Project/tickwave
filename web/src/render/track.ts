// One channel's data for the renderer. It keeps the last tile that arrived and fetches a new one
// when the view outgrows it; drawing never waits on a fetch. Until the new tile arrives, the old
// one draws at whatever scale it has, and a coarse tile of the whole channel fills what it leaves.

import {
  axisPeriod,
  channelStart,
  isAbort,
  type AnalogOverview,
  type Channel,
  type DigitalOverview,
  type Edges,
  type Samples,
  type Source,
} from "../source.ts";
import type { ValueRange } from "./scale.ts";
import { counts } from "./shade.ts";

export type Tile =
  | { readonly kind: "edges"; readonly start: number; readonly end: number; readonly data: Edges }
  | { readonly kind: "digital-overview"; readonly start: number; readonly end: number; readonly asked: number; readonly data: DigitalOverview }
  | { readonly kind: "samples"; readonly start: number; readonly end: number; readonly data: Samples }
  | { readonly kind: "analog-overview"; readonly start: number; readonly end: number; readonly asked: number; readonly data: AnalogOverview };

/** A digital row's tile of its finest overview level where its edges are `dense`. */
type Mode = "edges" | "overview" | "samples" | "dense";

/** A tile and the ticks it draws over. */
export interface Layer {
  readonly tile: Tile;
  readonly start: number;
  readonly end: number;
}

/** Ticks a value of the tile stands for: a bucket, or 0 where it holds every edge or sample. */
export function grain(t: Tile): number {
  return t.kind === "edges" || t.kind === "samples" ? 0 : t.data.bucket;
}

/** An analog row reads samples until a column holds this many. */
const SAMPLES_PER_COLUMN = 32;

/**
 * After a failed read, the row asks again no sooner than this. Every redraw asks, and a pointer
 * moving over the plot redraws, so without it a server that keeps failing gets a read a frame.
 */
const RETRY_MS = 3000;

/**
 * The most edges a digital row reads for a view. Past it, counted from the index first where the
 * source can, the row keeps its finest overview level, each bucket across the columns it spans,
 * until the view narrows enough: a burst far denser than the channel's average density, whose
 * finest level is coarse, never makes a zoom decode it whole. The view core's columns keep the same
 * bound (crates/view/src/query.rs `EDGES_AT_MOST`).
 */
export const EDGES_AT_MOST = 2 ** 21;

/** The most edges a bucket of each density can hold (shade.ts `counts`): Infinity for one it can't
 * read, so the row asks the index. */
const MOST = Float64Array.from({ length: 256 }, (_, d) => (d === 0 ? 0 : Number(counts(d)?.[1] ?? Infinity)));

type Pending = { mode: Mode; start: number; end: number; per: number; abort: AbortController; since: number };

/** `t` moved by `by` ticks. Samples go by index, so theirs move with the channel's first sample. */
function moved(t: Tile, by: number): Tile {
  switch (t.kind) {
    case "edges":
      return { ...t, start: t.start + by, end: t.end + by, data: { level: t.data.level, ticks: t.data.ticks.map((x) => x + by) } };
    case "samples":
      return { ...t, start: t.start + by, end: t.end + by };
    case "digital-overview":
      return { ...t, start: t.start + by, end: t.end + by, data: { ...t.data, start: t.data.start + by } };
    case "analog-overview":
      return { ...t, start: t.start + by, end: t.end + by, data: { ...t.data, start: t.data.start + by } };
  }
}

export class Track {
  readonly source: Source;
  readonly index: number;
  channel: Channel;
  tile: Tile | null = null;
  /** Whether the last read failed: cleared by the next that lands. */
  failed = false;
  /** The finest tile yet that covers the whole channel. */
  #base: Tile | null = null;
  #pending: Pending | null = null;
  #retryAt = 0;
  /** A request for buckets no larger than this came back null: there is no level that fine. */
  #noOverviewAtOrBelow = 0;
  /** When the base's read began, while it is out: until it lands, comes back null or fails. */
  #baseSince: number | null = null;
  /** `wholeRange`'s answer, and the base it came from. */
  #whole: { of: Tile; range: ValueRange } | null = null;
  /** The finest overview level's buckets, once a request has met it. */
  #finest = Infinity;
  /** The last read that would have held more than `EDGES_AT_MOST` edges: its span and the view's
   * ticks a column then. A view whose read lies inside it, at no more than twice as fine, is
   * `dense` too. */
  #dense: { start: number; end: number; per: number } | null = null;
  readonly #changed: () => void;
  readonly #failed: (e: unknown) => void;

  constructor(source: Source, index: number, channel: Channel, changed: () => void, failed: (e: unknown) => void) {
    this.source = source;
    this.index = index;
    this.channel = channel;
    this.#changed = changed;
    this.#failed = failed;
    // The base is asked for at once, not at the first draw: a first draw comes from it, and the
    // draw that asks would leave it a frame later. A channel this version can't read has none.
    if (channel.kind !== "unavailable") {
      this.#baseSince = performance.now();
      void this.#fetchBase();
    }
  }

  /** The channel moved by `by` ticks, now `channel`: the tiles in hand move with it, so the row
   * draws on while a read lands where the channel now is. One under way would land where it was. */
  moveBy(channel: Channel, by: number): void {
    this.channel = channel;
    if (this.tile) this.tile = moved(this.tile, by);
    if (this.#base) this.#base = moved(this.#base, by);
    this.#dense = null;
    this.#pending?.abort.abort();
    this.#pending = null;
  }

  /**
   * An analog channel's least and greatest values, from the base, the coarsest tile the row reads
   * that covers the channel: its overview's buckets, or its samples where it has no overview.
   * Every level's buckets hold the same least and greatest. Infinity and -Infinity where none holds
   * a value; null until the base lands.
   */
  wholeRange(): ValueRange | null {
    const b = this.#base;
    if (!b || this.channel.kind !== "analog") return null;
    if (this.#whole?.of === b) return this.#whole.range;
    let [lo, hi] = [Infinity, -Infinity];
    const [mins, maxes] = b.kind === "analog-overview" ? [b.data.min, b.data.max] : b.kind === "samples" ? [b.data.values, b.data.values] : [[], []];
    for (const v of mins) if (v < lo) lo = v;
    for (const v of maxes) if (v > hi) hi = v;
    this.#whole = { of: b, range: { lo, hi } };
    return this.#whole.range;
  }

  get busy(): boolean {
    return this.#pending !== null;
  }

  /** Whether the base's read is over: it landed, came back null (no overview) or failed. */
  get baseSettled(): boolean {
    return this.#baseSince === null;
  }

  /** When the read under way began, the base's included, in `performance.now()` time; null when
   * none is. */
  get waitingSince(): number | null {
    const [p, b] = [this.#pending?.since ?? null, this.#baseSince];
    return p === null ? b : b === null ? p : Math.min(p, b);
  }

  #mode(start: number, end: number, per: number): Mode {
    const c = this.channel;
    if (c.kind === "digital") {
      if (per > this.#noOverviewAtOrBelow) return "overview";
      const d = this.#dense;
      const [lo, hi] = this.#reads(start, end);
      return d && per > d.per / 2 && d.start <= lo && hi <= d.end ? "dense" : "edges";
    }
    if (c.kind === "unavailable") return "overview";
    const period = axisPeriod(c);
    if (per < SAMPLES_PER_COLUMN * period) return "samples";
    return per > this.#noOverviewAtOrBelow ? "overview" : "samples";
  }

  /** What a row reads for the view [start, end): a view's width either side, so a small pan draws
   * from what is here. */
  #reads(start: number, end: number): [number, number] {
    const width = end - start;
    return [Math.max(channelStart(this.channel), Math.floor(start - width)), Math.min(this.channel.end, Math.ceil(end + width))];
  }

  #covers(r: { readonly start: number; readonly end: number }, start: number, end: number): boolean {
    return r.start <= Math.max(channelStart(this.channel), start) && r.end >= Math.min(this.channel.end, end);
  }

  /** Whether `tile` is what the view [start, end) at `per` ticks a column wants. */
  #fits(tile: Tile, mode: Mode, start: number, end: number, per: number): boolean {
    if (!this.#covers(tile, start, end)) return false;
    if (mode === "dense") return tile.kind === "digital-overview";
    if (mode !== "overview") return tile.kind === mode;
    if (tile.kind !== "digital-overview" && tile.kind !== "analog-overview") return false;
    // Zoomed out far past what was asked for, a coarser level may exist.
    return tile.data.bucket <= per && per <= tile.asked * 4;
  }

  /**
   * What to draw: the tile over the ticks it covers, and the base over the rest. An overview
   * coarser than a column still draws, each bucket across the columns it spans.
   */
  layers(): Layer[] {
    const t = this.tile;
    const b = this.#base;
    const out: Layer[] = [];
    if (b && b !== t) {
      if (!t) out.push({ tile: b, start: b.start, end: b.end });
      else {
        if (t.start > b.start) out.push({ tile: b, start: b.start, end: t.start });
        if (t.end < b.end) out.push({ tile: b, start: t.end, end: b.end });
      }
    }
    if (t) out.push({ tile: t, start: t.start, end: t.end });
    return out;
  }

  /** Whether the current tile is the one this view wants: always, for a channel this version
   * can't read, which reads nothing. */
  settled(start: number, end: number, per: number): boolean {
    if (this.channel.kind === "unavailable") return true;
    return this.tile !== null && this.#fits(this.tile, this.#mode(start, end, per), start, end, per) && this.#pending === null;
  }

  /** Starts a fetch if the current tile and any pending one do not fit the view. */
  need(start: number, end: number, per: number): void {
    if (this.channel.kind === "unavailable") return;
    const mode = this.#mode(start, end, per);
    if (this.tile && this.#fits(this.tile, mode, start, end, per)) {
      this.#pending?.abort.abort();
      this.#pending = null;
      return;
    }
    const p = this.#pending;
    // A pending overview was asked for buckets no larger than p.per, so it fits a view at least that coarse.
    if (p && p.mode === mode && this.#covers(p, start, end) && (mode !== "overview" || (p.per <= per && per <= p.per * 4))) return;
    p?.abort.abort();
    this.#pending = null;
    if (performance.now() < this.#retryAt) return;
    const [lo, hi] = this.#reads(start, end);
    const abort = new AbortController();
    const pending = { mode, start: lo, end: hi, per, abort, since: performance.now() };
    this.#pending = pending;
    void this.#fetch(pending).catch((e) => {
      if (this.#pending === pending) this.#pending = null;
      if (isAbort(e)) return;
      this.#retryAt = performance.now() + RETRY_MS;
      this.failed = true;
      this.#failed(e);
    });
  }

  #offerBase(tile: Tile): void {
    if (!this.#covers(tile, channelStart(this.channel), this.channel.end)) return;
    if (this.#base && grain(this.#base) < grain(tile)) return;
    this.#base = tile;
  }

  /** At most how many edges [start, end) holds, by a digital overview in hand that covers it:
   * Infinity where none does. */
  #atMost(start: number, end: number): number {
    for (const t of [this.tile, this.#base]) {
      if (t?.kind !== "digital-overview" || !this.#covers(t, start, end)) continue;
      const { start: s0, bucket, density } = t.data;
      let n = 0;
      const last = Math.min(density.length, Math.ceil((end - s0) / bucket));
      for (let i = Math.max(0, Math.floor((start - s0) / bucket)); i < last && n <= EDGES_AT_MOST; i++) n += MOST[density[i]];
      return n;
    }
    return Infinity;
  }

  /** `ov`'s buckets, where it is the finest level met yet. */
  #met(ov: DigitalOverview | AnalogOverview): void {
    this.#finest = Math.min(this.#finest, ov.bucket);
  }

  /** The coarsest overview level, which is small: the base until a finer tile covers the channel. */
  async #fetchBase(): Promise<void> {
    try {
      const start = channelStart(this.channel);
      const end = this.channel.end;
      const ov = await this.source.overview(this.index, start, end, Infinity);
      this.#baseSince = null;
      if (!ov) {
        this.#changed();
        return;
      }
      this.#met(ov);
      this.#offerBase(
        ov.kind === "digital"
          ? { kind: "digital-overview", start, end, asked: Infinity, data: ov }
          : { kind: "analog-overview", start, end, asked: Infinity, data: ov },
      );
      this.failed = false;
      this.#changed();
    } catch (e) {
      this.#baseSince = null;
      if (isAbort(e)) return;
      this.failed = true;
      this.#failed(e);
    }
  }

  async #fetch(p: Pending): Promise<void> {
    const { signal } = p.abort;
    const done = (tile: Tile) => {
      if (this.#pending !== p) return;
      this.#pending = null;
      this.tile = tile;
      this.failed = false;
      this.#offerBase(tile);
      this.#changed();
    };
    if (p.mode === "overview") {
      const ov = await this.source.overview(this.index, p.start, p.end, p.per, signal);
      if (ov) {
        this.#met(ov);
        done(
          ov.kind === "digital"
            ? { kind: "digital-overview", start: p.start, end: p.end, asked: p.per, data: ov }
            : { kind: "analog-overview", start: p.start, end: p.end, asked: p.per, data: ov },
        );
        return;
      }
      this.#noOverviewAtOrBelow = Math.max(this.#noOverviewAtOrBelow, p.per);
      if (this.#pending !== p) return;
      p.mode = this.channel.kind === "digital" ? "edges" : "samples";
    }
    // The index's count, a round trip, only where the overview in hand can't rule the bound out.
    const over = p.mode === "edges" && this.#atMost(p.start, p.end) > EDGES_AT_MOST;
    if (over && this.source.edgeCount && (await this.source.edgeCount(this.index, p.start, p.end, signal)) > EDGES_AT_MOST) {
      this.#dense = { start: p.start, end: p.end, per: p.per };
      if (this.#pending !== p) return;
      p.mode = "dense";
    }
    if (p.mode === "dense") {
      // The finest level: the first a doubling bucket meets past the largest that met none, short
      // of the finest met yet (the base's, at first), else that one.
      let ov: DigitalOverview | AnalogOverview | null = null;
      for (let b = 2 * Math.max(1, this.#noOverviewAtOrBelow); !ov && b < this.#finest && b < 2 * this.channel.end; b *= 2) {
        ov = await this.source.overview(this.index, p.start, p.end, b, signal);
        if (!ov) this.#noOverviewAtOrBelow = Math.max(this.#noOverviewAtOrBelow, b);
      }
      ov ??= await this.source.overview(this.index, p.start, p.end, this.#finest, signal);
      if (ov?.kind === "digital") {
        this.#met(ov);
        done({ kind: "digital-overview", start: p.start, end: p.end, asked: ov.bucket, data: ov });
        return;
      }
      // No level at all: the edges, as many as they are.
      p.mode = "edges";
      this.#dense = null;
    }
    if (p.mode === "edges") {
      done({ kind: "edges", start: p.start, end: p.end, data: await this.source.edges(this.index, p.start, p.end, signal) });
    } else {
      done({ kind: "samples", start: p.start, end: p.end, data: await this.source.samples(this.index, p.start, p.end, signal) });
    }
  }
}
