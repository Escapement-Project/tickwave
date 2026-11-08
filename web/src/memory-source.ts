// A Source over a recording held in memory. It computes the overview on the fly, at the levels a
// real file's head would hold (FORMAT.md, "Overview"), so the renderer meets the same coarseness
// and the same gaps it will meet in a real file.

import { lowerBound } from "./search.ts";
import {
  aborted,
  sampleTick,
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
  /** Ascending, no two equal. Handed out by reference: never written after construction. */
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
 * FORMAT.md's rule: shifts 12, 16, 20, ... in steps of 4, keeping those with at most 2^17 buckets
 * and stopping at the first with at most 1,024. A level has (length >> shift) + 1 buckets.
 */
export function formatLevels(length: number): number[] {
  const levels: number[] = [];
  for (let shift = 12; shift < 63; shift += 4) {
    const buckets = Math.floor(length / 2 ** shift) + 1;
    if (buckets <= 2 ** 17) levels.push(2 ** shift);
    if (buckets <= 1024) break;
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

  samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    return this.#answer(signal, () => {
      const c = this.#analog(channel);
      const lo = sampleIndexAtOrAfter(c, start);
      const hi = Math.max(lo, sampleIndexAtOrAfter(c, end));
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
        const level = new Uint8Array(n);
        const changes = new Uint8Array(n);
        let i = lowerBound(c.ticks, s0);
        for (let k = 0; k < n; k++) {
          const b = s0 + k * bucket;
          i = lowerBound(c.ticks, b, i);
          level[k] = c.initial ^ (i & 1);
          changes[k] = i < c.ticks.length && c.ticks[i] < b + bucket ? 1 : 0;
        }
        return { kind: "digital", bucket, start: s0, level, changes };
      }
      const min = new Float64Array(n).fill(Infinity);
      const max = new Float64Array(n).fill(-Infinity);
      let i = sampleIndexAtOrAfter(c, s0);
      for (let k = 0; k < n; k++) {
        const next = sampleIndexAtOrAfter(c, s0 + (k + 1) * bucket);
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

/** The index of the first sample whose tick is at or after `tick`, clamped to [0, count]. */
export function sampleIndexAtOrAfter(
  c: { readonly first: number; readonly period: Ratio; readonly values: { readonly length: number } },
  tick: number,
): number {
  const { num, den } = c.period;
  let i = Math.ceil(((tick - c.first) * den) / num);
  // The estimate can land one off either way; settle it against the sample ticks themselves.
  i = Math.min(Math.max(i, 0), c.values.length);
  while (i > 0 && sampleTick(c.first, c.period, i - 1) >= tick) i--;
  while (i < c.values.length && sampleTick(c.first, c.period, i) < tick) i++;
  return i;
}
