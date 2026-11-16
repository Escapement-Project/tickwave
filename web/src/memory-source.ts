// A Source over a recording held in memory. It computes the overview on the fly, at the levels a
// real file's head would hold (FORMAT.md, "Overview"), so the renderer meets the same coarseness
// and the same gaps it will meet in a real file.

import { densityOf, dutyOf } from "./render/shade.ts";
import { lowerBound } from "./search.ts";
import {
  aborted,
  sampleIndexAtOrAfter,
  type Channel,
  type Edges,
  type Overview,
  type Ratio,
  type RecordingInfo,
  type Samples,
  type Source,
} from "./source.ts";

export interface MemoryDigital {
  readonly kind: "digital";
  readonly name: string;
  readonly end: number;
  /** The level at tick 0, before any edge. */
  readonly initial: 0 | 1;
  /** In order; two can share a tick. Handed out by reference: never written after construction. */
  readonly ticks: Float64Array;
}

export interface MemoryAnalog {
  readonly kind: "analog";
  readonly name: string;
  readonly end: number;
  readonly unit: string;
  readonly first: number;
  readonly period: Ratio;
  readonly resolution: number | null;
  readonly values: Float64Array;
}

export interface MemoryRecording {
  readonly title: string;
  readonly timebase: Ratio;
  readonly length: number;
  readonly bytes?: number;
  readonly channels: readonly (MemoryDigital | MemoryAnalog)[];
}

export interface MemorySourceOptions {
  /** Overview bucket sizes in ticks; by default the levels FORMAT.md keeps for this length. */
  readonly levels?: readonly number[];
  /** Delay every answer by this many milliseconds, to exercise the renderer's waiting. */
  readonly latency?: number;
}

/**
 * The levels a file of `length` ticks has, from FORMAT.md ("Overview"): its finest shift, then
 * steps of 4, stopping at the first with at most 1,024 buckets or at shift 62. Those with at most
 * 2^17 buckets are in the footer, finer ones in overview blocks. A level has
 * ceil(length / 2^shift) buckets. A memory source takes them from a finest shift of 12.
 */
export function formatLevels(length: number, finest = 12): number[] {
  const levels: number[] = [];
  for (let shift = finest; ; shift += 4) {
    levels.push(2 ** shift);
    if (Math.ceil(length / 2 ** shift) <= 1024 || shift + 4 > 62) break;
  }
  return levels;
}

export class MemorySource implements Source {
  readonly #rec: MemoryRecording;
  readonly #info: RecordingInfo;
  readonly #levels: readonly number[];
  readonly #latency: number;

  constructor(rec: MemoryRecording, options: MemorySourceOptions = {}) {
    this.#rec = rec;
    this.#levels = [...(options.levels ?? formatLevels(rec.length))].sort((a, b) => a - b);
    this.#latency = options.latency ?? 0;
    const channels: Channel[] = rec.channels.map((c) => {
      if (c.kind === "digital") return { kind: "digital", name: c.name, end: c.end };
      let min = Infinity;
      let max = -Infinity;
      for (const v of c.values) {
        if (v < min) min = v;
        if (v > max) max = v;
      }
      return {
        kind: "analog",
        name: c.name,
        end: c.end,
        unit: c.unit,
        first: c.first,
        period: c.period,
        count: c.values.length,
        resolution: c.resolution,
        min,
        max,
      };
    });
    this.#info = {
      title: rec.title,
      timebase: rec.timebase,
      length: rec.length,
      channels,
      ...(rec.bytes !== undefined && { bytes: rec.bytes }),
    };
  }

  async #answer<T>(signal: AbortSignal | undefined, f: () => T): Promise<T> {
    if (signal?.aborted) throw aborted();
    if (this.#latency > 0) await new Promise((r) => setTimeout(r, this.#latency));
    if (signal?.aborted) throw aborted();
    return f();
  }

  #digital(channel: number): MemoryDigital {
    const c = this.#rec.channels[channel];
    if (c?.kind !== "digital") throw new RangeError(`channel ${channel} is not digital`);
    return c;
  }

  #analog(channel: number): MemoryAnalog {
    const c = this.#rec.channels[channel];
    if (c?.kind !== "analog") throw new RangeError(`channel ${channel} is not analog`);
    return c;
  }

  info(signal?: AbortSignal): Promise<RecordingInfo> {
    return this.#answer(signal, () => this.#info);
  }

  prefetch(): Promise<void> {
    return Promise.resolve();
  }

  edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges> {
    return this.#answer(signal, () => {
      const c = this.#digital(channel);
      const lo = lowerBound(c.ticks, start);
      const hi = Math.max(lo, lowerBound(c.ticks, end, lo));
      return { level: (c.initial ^ (lo & 1)) as 0 | 1, ticks: c.ticks.subarray(lo, hi) };
    });
  }

  edgeCount(channel: number, start: number, end: number, signal?: AbortSignal): Promise<number> {
    return this.#answer(signal, () => {
      const t = this.#digital(channel).ticks;
      const lo = lowerBound(t, start);
      return Math.max(0, lowerBound(t, end, lo) - lo);
    });
  }

  samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    return this.#answer(signal, () => {
      const c = this.#analog(channel);
      const lo = sampleIndexAtOrAfter(c.first, c.period, c.values.length, start);
      const hi = Math.max(lo, sampleIndexAtOrAfter(c.first, c.period, c.values.length, end));
      return { first: lo, values: c.values.subarray(lo, hi) };
    });
  }

  overview(
    channel: number,
    start: number,
    end: number,
    maxBucket: number,
    signal?: AbortSignal,
  ): Promise<Overview | null> {
    return this.#answer(signal, () => {
      let bucket = 0;
      for (const b of this.#levels) if (b <= maxBucket) bucket = b;
      if (bucket === 0) return null;
      const c = this.#rec.channels[channel];
      const s0 = Math.floor(Math.max(0, start) / bucket) * bucket;
      const n = Math.max(0, Math.ceil((Math.min(end, c.end) - s0) / bucket));
      if (c.kind === "digital") {
        const density = new Uint8Array(n);
        const duty = new Uint8Array(n);
        const t = c.ticks;
        let i = lowerBound(t, s0);
        for (let k = 0; k < n; k++) {
          const b0 = s0 + k * bucket;
          // The channel's kept span runs from its start, tick 0, to the length.
          const b1 = Math.min(b0 + bucket, this.#rec.length);
          const j = lowerBound(t, b0 + bucket, i);
          density[k] = densityOf(j - i);
          // The level at a tick is taken through every edge at or before it.
          let level = c.initial ^ (i & 1);
          let at = b0;
          let high = 0;
          for (let e = i; e < j; e++) {
            if (level) high += t[e] - at;
            at = t[e];
            level ^= 1;
          }
          if (level) high += b1 - at;
          duty[k] = dutyOf(high, b1 - b0);
          i = j;
        }
        return { kind: "digital", bucket, start: s0, density, duty };
      }
      const min = new Float64Array(n).fill(Infinity);
      const max = new Float64Array(n).fill(-Infinity);
      const index = (tick: number) => sampleIndexAtOrAfter(c.first, c.period, c.values.length, tick);
      let i = index(s0);
      for (let k = 0; k < n; k++) {
        const next = index(s0 + (k + 1) * bucket);
        for (; i < next; i++) {
          const v = c.values[i];
          if (v < min[k]) min[k] = v;
          if (v > max[k]) max[k] = v;
        }
      }
      return { kind: "analog", bucket, start: s0, min, max };
    });
  }
}

