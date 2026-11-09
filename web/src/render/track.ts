// One channel's data for the renderer. It keeps the last tile that arrived and fetches a new one
// when the view outgrows it; drawing never waits on a fetch. Until the new tile arrives, the old
// one draws at whatever scale it has, and a coarse tile of the whole channel fills what it leaves.

import {
  isAbort,
  type AnalogOverview,
  type Channel,
  type DigitalOverview,
  type Edges,
  type Samples,
  type Source,
} from "../source.ts";

export type Tile =
  | { readonly kind: "edges"; readonly start: number; readonly end: number; readonly data: Edges }
  | { readonly kind: "digital-overview"; readonly start: number; readonly end: number; readonly asked: number; readonly data: DigitalOverview }
  | { readonly kind: "samples"; readonly start: number; readonly end: number; readonly data: Samples }
  | { readonly kind: "analog-overview"; readonly start: number; readonly end: number; readonly asked: number; readonly data: AnalogOverview };

type Mode = "edges" | "overview" | "samples";

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

export class Track {
  readonly source: Source;
  readonly index: number;
  readonly channel: Channel;
  tile: Tile | null = null;
  /** The finest tile yet that covers the whole channel. */
  #base: Tile | null = null;
  #baseAsked = false;
  #pending: { mode: Mode; start: number; end: number; per: number; abort: AbortController } | null = null;
  /** A request for buckets no larger than this came back null: there is no level that fine. */
  #noOverviewAtOrBelow = 0;
  readonly #changed: () => void;
  readonly #failed: (e: unknown) => void;

  constructor(source: Source, index: number, channel: Channel, changed: () => void, failed: (e: unknown) => void) {
    this.source = source;
    this.index = index;
    this.channel = channel;
    this.#changed = changed;
    this.#failed = failed;
  }

  get busy(): boolean {
    return this.#pending !== null;
  }

  #mode(per: number): Mode {
    const c = this.channel;
    if (c.kind === "digital") return per > this.#noOverviewAtOrBelow ? "overview" : "edges";
    const period = c.period.num / c.period.den;
    if (per < SAMPLES_PER_COLUMN * period) return "samples";
    return per > this.#noOverviewAtOrBelow ? "overview" : "samples";
  }

  #covers(r: { readonly start: number; readonly end: number }, start: number, end: number): boolean {
    return r.start <= Math.max(0, start) && r.end >= Math.min(this.channel.end, end);
  }

  /** Whether `tile` is what the view [start, end) at `per` ticks a column wants. */
  #fits(tile: Tile, mode: Mode, start: number, end: number, per: number): boolean {
    if (!this.#covers(tile, start, end)) return false;
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

  /** Whether the current tile is the one this view wants. */
  settled(start: number, end: number, per: number): boolean {
    return this.tile !== null && this.#fits(this.tile, this.#mode(per), start, end, per) && this.#pending === null;
  }

  /** Starts a fetch if the current tile and any pending one do not fit the view. */
  need(start: number, end: number, per: number): void {
    if (!this.#baseAsked) {
      this.#baseAsked = true;
      void this.#fetchBase();
    }
    const mode = this.#mode(per);
    if (this.tile && this.#fits(this.tile, mode, start, end, per)) {
      this.#pending?.abort.abort();
      this.#pending = null;
      return;
    }
    const p = this.#pending;
    // A pending overview was asked for buckets no larger than p.per, so it fits a view at least that coarse.
    if (p && p.mode === mode && this.#covers(p, start, end) && (mode !== "overview" || (p.per <= per && per <= p.per * 4))) return;
    p?.abort.abort();
    // Fetch a view's width either side, so a small pan draws from what is here.
    const width = end - start;
    const lo = Math.max(0, Math.floor(start - width));
    const hi = Math.min(this.channel.end, Math.ceil(end + width));
    const abort = new AbortController();
    const pending = { mode, start: lo, end: hi, per, abort };
    this.#pending = pending;
    void this.#fetch(pending).catch((e) => {
      if (this.#pending === pending) this.#pending = null;
      if (!isAbort(e)) this.#failed(e);
    });
  }

  #offerBase(tile: Tile): void {
    if (!this.#covers(tile, 0, this.channel.end)) return;
    if (this.#base && grain(this.#base) < grain(tile)) return;
    this.#base = tile;
  }

  /** The coarsest overview level, which is small: the base until a finer tile covers the channel. */
  async #fetchBase(): Promise<void> {
    try {
      const ov = await this.source.overview(this.index, 0, this.channel.end, Infinity);
      if (!ov) return;
      const end = this.channel.end;
      this.#offerBase(
        ov.kind === "digital"
          ? { kind: "digital-overview", start: 0, end, asked: Infinity, data: ov }
          : { kind: "analog-overview", start: 0, end, asked: Infinity, data: ov },
      );
      this.#changed();
    } catch (e) {
      if (!isAbort(e)) this.#failed(e);
    }
  }

  async #fetch(p: { mode: Mode; start: number; end: number; per: number; abort: AbortController }): Promise<void> {
    const { signal } = p.abort;
    const done = (tile: Tile) => {
      if (this.#pending !== p) return;
      this.#pending = null;
      this.tile = tile;
      this.#offerBase(tile);
      this.#changed();
    };
    if (p.mode === "overview") {
      const ov = await this.source.overview(this.index, p.start, p.end, p.per, signal);
      if (ov) {
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
    if (p.mode === "edges") {
      done({ kind: "edges", start: p.start, end: p.end, data: await this.source.edges(this.index, p.start, p.end, signal) });
    } else {
      done({ kind: "samples", start: p.start, end: p.end, data: await this.source.samples(this.index, p.start, p.end, signal) });
    }
  }
}
