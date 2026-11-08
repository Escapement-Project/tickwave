// One channel's data for the renderer. It keeps the last tile that arrived and fetches a new one
// when the view outgrows it; drawing never waits on a fetch.

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

/** An analog row reads samples until a column holds this many. */
const SAMPLES_PER_COLUMN = 32;

export class Track {
  readonly source: Source;
  readonly index: number;
  readonly channel: Channel;
  tile: Tile | null = null;
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

  /** The tile to draw with at `per` ticks a column, or nothing yet. */
  drawable(per: number): Tile | null {
    const t = this.tile;
    if (!t) return null;
    // Edges and samples are exact at any scale; an overview only where a column holds its buckets.
    if (t.kind === "edges" || t.kind === "samples") return t;
    return t.data.bucket <= per ? t : null;
  }

  /** Whether the current tile is the one this view wants. */
  settled(start: number, end: number, per: number): boolean {
    return this.tile !== null && this.#fits(this.tile, this.#mode(per), start, end, per) && this.#pending === null;
  }

  /** Starts a fetch if the current tile and any pending one do not fit the view. */
  need(start: number, end: number, per: number): void {
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

  async #fetch(p: { mode: Mode; start: number; end: number; per: number; abort: AbortController }): Promise<void> {
    const { signal } = p.abort;
    const done = (tile: Tile) => {
      if (this.#pending !== p) return;
      this.#pending = null;
      this.tile = tile;
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
