// The row under the pointer, read from the data the row has drawn: an analog value, or the range one
// pixel draws; a digital pulse's level, width, period, frequency and duty, its spans marked on the
// trace. A pulse whose edges lie past the data drawn is finished once the pointer rests within it.
// The view's label and the inset's reading line both show what it reads.

import {
  analogFromOverview,
  analogFromSamples,
  complete,
  completePulse,
  denseFigures,
  denseFromEdges,
  denseFromOverview,
  knownSpan,
  pulseFigures,
  pulseFromEdges,
  type Pulse,
  type PulseFigures,
} from "./probe.ts";
import { formatValue } from "./readouts.ts";
import type { Plot, PulseMark } from "./render/plot.ts";
import type { Tile } from "./render/track.ts";
import { lowerBound } from "./search.ts";
import { inChannel, readingStep, rebased, spanned, type DigitalOverview, type RecordingInfo, type Source } from "./source.ts";

/** A pulse whose edges lie past the data drawn is measured once the pointer rests this long. */
const REST_MS = 150;

/** The reading where one pixel holds several edges, after its edges and its share of time high. */
export const ZOOM_IN = "zoom in to measure";

/** Bucket `k`'s level where it holds one throughout, as far as its duty says: 0 or 1, or null for
 * one that holds an edge, changes level, or lies outside the channel's kept span. */
function quietLevel(o: DigitalOverview, k: number): 0 | 1 | null {
  if (o.density[k] !== 0) return null;
  return o.duty[k] === 0 ? 0 : o.duty[k] === 14 ? 1 : null;
}

export type Probed =
  /** An analog value or range, a level while its pulse is measured, or a request to zoom in. */
  | { readonly kind: "text"; readonly row: number; readonly lines: readonly string[] }
  /** A whole pulse, marked on the trace; `on` says which of its figures sit on their spans. */
  | {
      readonly kind: "pulse";
      readonly row: number;
      readonly pulse: Pulse;
      readonly figures: PulseFigures;
      readonly mark: PulseMark | null;
      readonly on: { readonly width: boolean; readonly period: boolean };
    };

/** Whether a measured pulse holds `tick`. */
function within(p: Pulse, tick: number): boolean {
  return (p.start == null || p.start <= tick) && (p.end == null || tick < p.end);
}

/** Whether ticks `a` and `b` lie in one pulse, as far as `tile` shows: no edge between them. */
function onePulse(tile: Tile, a: number, b: number): boolean {
  const [lo, hi] = a < b ? [a, b] : [b, a];
  if (lo < tile.start || hi >= tile.end) return false;
  if (tile.kind === "edges") {
    // An edge at tick e starts the level after it: none in (lo, hi].
    const t = tile.data.ticks;
    return lowerBound(t, hi + 1) === lowerBound(t, lo + 1);
  }
  if (tile.kind === "digital-overview") {
    const o = tile.data;
    const k0 = Math.floor((lo - o.start) / o.bucket);
    const k1 = Math.floor((hi - o.start) / o.bucket);
    if (k0 < 0 || k1 >= o.density.length) return false;
    const level = quietLevel(o, k0);
    for (let k = k0; k <= k1; k++) if (level === null || quietLevel(o, k) !== level) return false;
    return true;
  }
  return false;
}

export class Prober {
  /** The mouse or pen over the plot, in the plot's coordinates. */
  pointer: { x: number; y: number } | null = null;
  readonly #plot: Plot;
  readonly #loaded: () => { readonly source: Source; readonly info: RecordingInfo } | null;
  readonly #show: (r: Probed | null) => void;
  readonly #held: () => boolean;
  #timer = 0;
  #abort: AbortController | null = null;
  /** The pulse being measured: it goes on while the pointer moves within it. */
  #measuring: { source: Source; index: number; tick: number; level: 0 | 1 } | null = null;
  /** The last pulse measured: it reads at once anywhere within it. */
  #measured: { source: Source; index: number; pulse: Pulse } | null = null;
  /** What it last read from: a draw that changes none of it leaves the reading be. */
  #from: { x: number; y: number; start: number; end: number; tile: unknown } | null = null;

  /**
   * `show` gets each reading, and null when there is none. While `held` says so, as in a drag, it
   * reads nothing.
   */
  constructor(
    plot: Plot,
    loaded: () => { readonly source: Source; readonly info: RecordingInfo } | null,
    show: (r: Probed | null) => void,
    held: () => boolean = () => false,
  ) {
    this.#plot = plot;
    this.#loaded = loaded;
    this.#show = show;
    this.#held = held;
  }

  /** Reads at the pointer. */
  probe(): void {
    // A measurement under way goes on only while the pointer stays within its pulse.
    let measuring = false;
    try {
      measuring = this.#at();
    } finally {
      if (!measuring) this.#stop();
    }
  }

  /** Reads again when a draw has brought new data or a new view under the pointer. */
  probeIfMoved(): void {
    const p = this.pointer;
    const f = this.#from;
    if (!p || !f) return;
    const row = this.#plot.hit(p.x, p.y);
    const tile = row.area === "row" ? this.#plot.tracks[row.row]?.tile : null;
    if (f.x !== p.x || f.y !== p.y || f.start !== this.#plot.view.start || f.end !== this.#plot.view.end || f.tile !== tile) this.probe();
  }

  hide(): void {
    this.#stop();
    this.#from = null;
    this.#set(null);
  }

  #stop(): void {
    clearTimeout(this.#timer);
    this.#abort?.abort();
    this.#abort = null;
    this.#measuring = null;
  }

  #set(r: Probed | null): void {
    const m = r?.kind === "pulse" ? r.mark : null;
    const o = this.#plot.pulseMark;
    const same = o === m || (o !== null && m !== null && (Object.keys(m) as (keyof PulseMark)[]).every((k) => o[k] === m[k]));
    if (!same) {
      this.#plot.pulseMark = m;
      this.#plot.invalidate();
    }
    this.#show(r);
  }

  /** True while it waits on a measurement of the pulse at the pointer. */
  #at(): boolean {
    const p = this.pointer;
    const l = this.#plot.layout;
    const loaded = this.#loaded();
    const hit = p && l ? this.#plot.hit(p.x, p.y) : null;
    if (!p || !l || !loaded || !hit || hit.area !== "row" || this.#held() || p.x < l.plotLeft || p.x >= l.plotRight) {
      this.#from = null;
      this.#set(null);
      return false;
    }
    const row = hit.row;
    const track = this.#plot.tracks[row];
    const tile = track?.tile ?? null;
    this.#from = { x: p.x, y: p.y, start: this.#plot.view.start, end: this.#plot.view.end, tile };
    const col = Math.floor(p.x - l.plotLeft) + l.plotLeft;
    const t0 = this.#plot.tickAt(col);
    const t1 = this.#plot.tickAt(col + 1);
    const tick = Math.floor(this.#plot.tickAt(p.x));
    // A channel whose list is in pages, as the tile's span sees it.
    const c = spanned(track.channel, tile?.data.listed);
    if (!tile || tick < tile.start || tick >= tile.end || !inChannel(c, tick)) {
      this.#set(null);
      return false;
    }
    const text = (lines: string[]) => this.#set({ kind: "text", row, lines });
    if (c.kind === "analog") {
      const at = tile.kind === "samples" ? analogFromSamples(c, tile.data, t0, t1, tick) : tile.kind === "analog-overview" ? analogFromOverview(tile.data, t0, t1) : null;
      const v = (x: number) => formatValue(x, c.resolution, c.unit, c.significant);
      if (at?.kind === "value") text([v(at.value)]);
      else if (at?.kind === "range") text([`${v(at.min)} to ${v(at.max)}`]);
      else this.#set(null);
      return false;
    }
    let pulse: Pulse | null = null;
    const step = readingStep(loaded.info, [track.index]);
    if (tile.kind === "edges") {
      const from = rebased(c, tile.start, tile.data, tick);
      const t = tile.data.ticks;
      // Two edges in the pixel: no one pulse is under the pointer.
      if (lowerBound(t, Math.ceil(t1)) - lowerBound(t, Math.ceil(t0)) > 1) {
        text([...denseFigures(denseFromEdges(from.edges, t0, t1, knownSpan(c, tick)), loaded.info.timebase, step), ZOOM_IN]);
        return false;
      }
      pulse = pulseFromEdges(from.edges, from.start, tile.end, knownSpan(c, tick), tick);
    } else if (tile.kind === "digital-overview") {
      const o = tile.data;
      const k0 = Math.max(0, Math.floor((t0 - o.start) / o.bucket));
      const k1 = Math.min(o.density.length, Math.ceil((t1 - o.start) / o.bucket));
      const level = k0 < k1 ? quietLevel(o, k0) : null;
      let quiet = level !== null;
      for (let k = k0 + 1; k < k1 && quiet; k++) quiet = quietLevel(o, k) === level;
      if (!quiet) {
        text([...denseFigures(denseFromOverview(o, t0, t1, loaded.info.length), loaded.info.timebase, step), ZOOM_IN]);
        return false;
      }
      pulse = { level: level!, start: undefined, end: undefined, next: undefined };
    }
    if (!pulse) {
      this.#set(null);
      return false;
    }
    const known = this.#measured;
    if (!complete(pulse) && known?.source === loaded.source && known.index === track.index && within(known.pulse, tick)) pulse = known.pulse;
    if (complete(pulse)) {
      const figures = pulseFigures(pulse, loaded.info.timebase, knownSpan(c, tick), step);
      const { start, end } = pulse;
      const mark: PulseMark | null =
        typeof start === "number" && typeof end === "number"
          ? { row, level: pulse.level, start, end, next: pulse.next ?? null, width: figures.width, period: figures.period }
          : null;
      this.#set({ kind: "pulse", row, pulse, figures, mark, on: mark ? this.#plot.markFits(mark) : { width: false, period: false } });
      return false;
    }
    // A row whose last read failed waits before it reads again, and so does the reading: the level only.
    if (track.failed) {
      text([pulse.level ? "high" : "low"]);
      return false;
    }
    const m = this.#measuring;
    const same = m?.source === loaded.source && m.index === track.index && m.level === pulse.level && onePulse(tile, m.tick, tick);
    if (!same) {
      this.#stop();
      const asked = pulse;
      const abort = new AbortController();
      this.#abort = abort;
      this.#measuring = { source: loaded.source, index: track.index, tick, level: pulse.level };
      this.#timer = window.setTimeout(() => {
        completePulse(loaded.source, track.index, asked, tick, knownSpan(c, tick), abort.signal).then(
          (done) => {
            if (abort.signal.aborted) return;
            this.#measuring = null;
            this.#abort = null;
            this.#measured = { source: loaded.source, index: track.index, pulse: done };
            // Wherever the pointer has moved to since, within the pulse or not.
            this.probe();
          },
          // A measurement that fails says nothing: the row's own read reports the trouble.
          () => {
            if (!abort.signal.aborted) this.#show(null);
          },
        );
      }, REST_MS);
    }
    text([pulse.level ? "high" : "low", "measuring…"]);
    return true;
  }
}
