// The renderer's only view of a recording. Every method is async: blocks arrive from a worker and
// the network, and the renderer draws what it has and redraws when more arrives.
//
// Channels are named by their index in `RecordingInfo.channels`. Ticks are integers below 2^53,
// counted from the capture's start; ranges are half-open, [start, end).

/** A positive rational number, num / den. */
export interface Ratio {
  readonly num: number;
  readonly den: number;
}

export interface DigitalChannel {
  readonly kind: "digital";
  readonly name: string;
  /** The first tick past the channel's data. Nothing is drawn at or after it. */
  readonly end: number;
}

export interface AnalogChannel {
  readonly kind: "analog";
  readonly name: string;
  readonly end: number;
  /** The unit of a value, such as "V" or "A". */
  readonly unit: string;
  /** The tick of sample 0. Sample i sits at first + floor(i * period). */
  readonly first: number;
  /** Ticks between samples. */
  readonly period: Ratio;
  readonly count: number;
  /**
   * The value one code step stands for, where the source recorded integer codes; readouts stop at
   * that digit. Null for a source that stored floating-point values.
   */
  readonly resolution: number | null;
  /** The smallest and largest value in the recording. */
  readonly min: number;
  readonly max: number;
}

export type Channel = DigitalChannel | AnalogChannel;

export interface RecordingInfo {
  readonly title: string;
  /** Ticks a second. */
  readonly timebase: Ratio;
  /** The capture covers ticks [0, length). */
  readonly length: number;
  readonly channels: readonly Channel[];
  /** The recording's download size in bytes, where the source knows it. */
  readonly bytes?: number;
}

export interface Edges {
  /** The level at `start`. */
  readonly level: 0 | 1;
  /** Every edge in [start, end), ascending. */
  readonly ticks: Float64Array;
}

/** Buckets of `bucket` ticks, the first starting at `start`, a multiple of `bucket`. */
export interface DigitalOverview {
  readonly kind: "digital";
  readonly bucket: number;
  readonly start: number;
  /** The level at each bucket's start. */
  readonly level: Uint8Array;
  /** 1 where the level changes inside the bucket. */
  readonly changes: Uint8Array;
}

export interface AnalogOverview {
  readonly kind: "analog";
  readonly bucket: number;
  readonly start: number;
  /** The smallest and largest value in each bucket; +Infinity and -Infinity in one with no sample. */
  readonly min: Float64Array | Float32Array;
  readonly max: Float64Array | Float32Array;
}

export type Overview = DigitalOverview | AnalogOverview;

export interface Samples {
  /** The index of `values[0]`. */
  readonly first: number;
  readonly values: Float64Array | Float32Array;
}

export interface Source {
  info(signal?: AbortSignal): Promise<RecordingInfo>;

  edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges>;

  /**
   * The coarsest overview level whose bucket is no larger than `maxBucket` ticks, covering
   * [start, end). Null when even the finest level is coarser than that, or the source has none:
   * the caller then reads edges or samples.
   */
  overview(
    channel: number,
    start: number,
    end: number,
    maxBucket: number,
    signal?: AbortSignal,
  ): Promise<Overview | null>;

  /** An analog channel's samples whose ticks fall in [start, end). */
  samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples>;

  /** Fetch what `info` and the coarse overview need, ahead of a view. Never rejects. */
  prefetch(): Promise<void>;
}

/** The tick of sample i: first + floor(i * period), as FORMAT.md places it. */
export function sampleTick(first: number, period: Ratio, i: number): number {
  const product = i * period.num;
  if (Number.isSafeInteger(product)) return first + Math.floor(product / period.den);
  return first + Number((BigInt(i) * BigInt(period.num)) / BigInt(period.den));
}

/** The DOMException a source rejects with when its signal aborts. */
export function aborted(): DOMException {
  return new DOMException("aborted", "AbortError");
}

export function isAbort(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}
