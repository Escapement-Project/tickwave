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
  /** The source's own name for the channel, as the header's `source_name` gives it. */
  readonly sourceName?: string;
  /** The first tick whose level is known, where the channel's data starts after the capture's
   * does. Absent: 0. Nothing is drawn before it. */
  readonly start?: number;
  /** The first tick past the channel's data. Nothing is drawn at or after it. */
  readonly end: number;
  /** Spans after `start` where the level was not known, in order (FORMAT.md, "Gaps"). */
  readonly gaps?: readonly Gap[];
}

/** Ticks `from..to` where a digital channel's level was not known; from `to` on it is `level`. */
export interface Gap {
  readonly from: number;
  readonly to: number;
  readonly level: 0 | 1;
}

/**
 * Sample `index` starts a new run at `tick`, after `lost` samples the instrument lost: 0 when it
 * paused (FORMAT.md, "Gaps").
 */
export interface Break {
  readonly index: number;
  readonly tick: number;
  readonly lost: number;
}

export interface AnalogChannel {
  readonly kind: "analog";
  readonly name: string;
  /** The source's own name for the channel, as the header's `source_name` gives it. */
  readonly sourceName?: string;
  readonly end: number;
  /** The unit of a value, such as "V" or "A". */
  readonly unit: string;
  /** The tick of sample 0. Sample i sits at first + floor(i * period), or after a break, at the
   * break's tick plus floor((i - index) * period). */
  readonly first: number;
  /** Ticks between samples. */
  readonly period: Ratio;
  readonly count: number;
  /** Where the samples start again after a stop, in order. */
  readonly breaks?: readonly Break[];
  /**
   * The value one code step stands for, where the source recorded integer codes; readouts stop at
   * that digit. Null for a source that stored floating-point values.
   */
  readonly resolution: number | null;
  /** At most this many significant figures in a reading, where the channel states it: an instrument
   * whose step changes with its range. */
  readonly significant?: number;
  /** The smallest and largest value in the recording. */
  readonly min: number;
  readonly max: number;
  /** A derived channel: the product of these two channels' values, by name. The file stores none
   * of its samples, and it sits where they do. */
  readonly product?: readonly [string, string];
}

/** What a channel moved or trimmed when it was converted states (FORMAT.md, "Moved and trimmed
 * channels"). */
export interface Shaped {
  /** Ticks it was moved by, later when positive, and why: `"delay"` is a known delay in its path. */
  readonly offset?: { readonly ticks: number; readonly kind: string };
  /** The ticks `from..to` where it holds what the source recorded; outside them it was cut. */
  readonly kept?: { readonly from: number; readonly to: number };
}

/** Which of the recording's sources a channel came from: its index in `RecordingInfo.sources`. */
export interface Sourced {
  readonly source?: number;
}

export type Channel = (DigitalChannel | AnalogChannel) & Shaped & Sourced;

/** The first tick the channel can hold data at: a digital channel's `start` (0 when absent), an
 * analog channel's first sample. */
export function channelStart(c: Channel): number {
  return c.kind === "digital" ? (c.start ?? 0) : c.first;
}

/** Whether `tick` lies in the channel's data: ticks `channelStart(c)..end`, outside its gaps. */
export function inChannel(c: Channel, tick: number): boolean {
  return tick >= channelStart(c) && tick < c.end && (c.kind !== "digital" || gapAt(c, tick) === null);
}

/** The gap holding `tick`, or null. */
export function gapAt(c: DigitalChannel, tick: number): Gap | null {
  for (const g of c.gaps ?? []) {
    if (tick < g.from) break;
    if (tick < g.to) return g;
  }
  return null;
}

/**
 * The ticks `[from, to)` of a channel's data, clipped to `[start, end)`: a digital channel's split
 * around its gaps, an analog channel's into its runs, each from its first sample to the place after
 * its last on its grid, or to the next run's start if that comes sooner.
 */
export function knownSpans(c: Channel, start: number, end: number): [number, number][] {
  const spans: [number, number][] = [];
  const stop = Math.min(end, c.end);
  const keep = (a: number, b: number) => {
    const [s, e] = [Math.max(a, start, channelStart(c)), Math.min(b, stop)];
    if (e > s) spans.push([s, e]);
  };
  if (c.kind === "analog") {
    const runs = c.breaks ?? [];
    for (let k = 0; k <= runs.length; k++) {
      const from = k === 0 ? c.first : runs[k - 1].tick;
      const last = (k < runs.length ? runs[k].index : c.count) - 1;
      const to = k < runs.length ? Math.min(runs[k].tick, placeAfter(c, last)) : c.end;
      keep(from, to);
    }
    return spans;
  }
  let at = -Infinity;
  for (const g of c.gaps ?? []) {
    keep(at, g.from);
    at = g.to;
  }
  keep(at, Infinity);
  return spans;
}

/**
 * Edges read from `start`, as they stand past the last gap that ends after `start` and at or before
 * `from`: no edge lies in a gap, and its end sets the level. Unchanged where no gap ends there.
 */
export function rebased(c: Channel, start: number, e: Edges, from: number): { start: number; edges: Edges } {
  let gap: Gap | null = null;
  for (const g of c.kind === "digital" ? (c.gaps ?? []) : []) if (g.to > start && g.to <= from) gap = g;
  if (!gap) return { start, edges: e };
  let lo = 0;
  while (lo < e.ticks.length && e.ticks[lo] < gap.to) lo++;
  return { start: gap.to, edges: { level: gap.level, ticks: e.ticks.subarray(lo) } };
}

/** How many of `runs` have `key` at or before `x`: they come in order, so these are the first.
 * A NaN `x` reads as past every run, as a walk back from the last leaves it. */
function runsUpTo(runs: readonly Break[], key: (b: Break) => number, x: number): number {
  if (Number.isNaN(x)) return runs.length;
  let lo = 0;
  let hi = runs.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (key(runs[mid]) <= x) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/** The tick of sample `i` of an analog channel, its breaks included. */
export function channelSampleTick(c: Pick<AnalogChannel, "first" | "period" | "breaks">, i: number): number {
  const runs = c.breaks ?? [];
  const k = runsUpTo(runs, (b) => b.index, i);
  return k === 0 ? sampleTick(c.first, c.period, i) : sampleTick(runs[k - 1].tick, c.period, i - runs[k - 1].index);
}

/** The place after sample `i` on its run's grid: where its time ends, unless the next run or the
 * length comes sooner (FORMAT.md, "Gaps"). */
export function placeAfter(c: Pick<AnalogChannel, "first" | "period" | "breaks">, i: number): number {
  const runs = c.breaks ?? [];
  const k = runsUpTo(runs, (b) => b.index, i);
  return k === 0 ? sampleTick(c.first, c.period, i + 1) : sampleTick(runs[k - 1].tick, c.period, i + 1 - runs[k - 1].index);
}

/** The index of the first sample at or after `tick`, breaks included, clamped to `0..=count`. */
export function channelIndexAtOrAfter(c: Pick<AnalogChannel, "first" | "period" | "count" | "breaks">, tick: number): number {
  const runs = c.breaks ?? [];
  // The run whose start is the last at or before `tick`; its samples end where the next run's start.
  const k = runsUpTo(runs, (b) => b.tick, tick);
  const from = k === 0 ? 0 : runs[k - 1].index;
  const to = k < runs.length ? runs[k].index : c.count;
  const start = k === 0 ? c.first : runs[k - 1].tick;
  return from + sampleIndexAtOrAfter(start, c.period, to - from, tick);
}

/** The span each break's lost samples cover, as FORMAT.md words it: from place `index` to place
 * `index + lost` on the grid of the run before the break, ending at the break's tick if that comes
 * sooner. Whole ticks. */
export function lostSpans(c: AnalogChannel): { from: number; to: number; lost: number }[] {
  const runs = c.breaks ?? [];
  return runs.flatMap((b, k) => {
    if (b.lost === 0) return [];
    const [index, tick] = k === 0 ? [0, c.first] : [runs[k - 1].index, runs[k - 1].tick];
    const place = (i: number) => Math.min(b.tick, sampleTick(tick, c.period, i - index));
    return [{ from: place(b.index), to: place(b.index + b.lost), lost: b.lost }];
  });
}

/**
 * The recording's ticks one tick of a reading's source spans (FORMAT.md, "Sources"): a reading of
 * channels keeps to the coarsest of their sources' ticks, and one tied to no channel to the finest
 * source's. 1 for a recording of one source, or one that lists none.
 */
export function readingStep(info: RecordingInfo, channels: readonly number[] = []): number {
  const sources = info.sources;
  if (!sources || sources.length < 2) return 1;
  // The recording's rate over the source's: a whole number by the format's rule.
  const factor = (s: number) => Math.round((info.timebase.num * sources[s].timebase[1]) / (info.timebase.den * sources[s].timebase[0]));
  const of = channels.map((c) => info.channels[c].source).filter((s): s is number => s !== undefined);
  return of.length > 0 ? Math.max(...of.map(factor)) : Math.min(...sources.map((_, s) => factor(s)));
}

/** A recording's content id: "blake3:" and the 64 hex digits of its content's digest. */
export type ContentId = `blake3:${string}`;

/** The kind of capture a recording was converted from. A newer converter may write another name. */
export type SourceFormat = "saleae-binary-0" | "saleae-binary-1" | "sigrok-sr" | "vcd" | (string & {});

/** How the converter came by the sample rate, where the source doesn't record it. */
export type RateOrigin = "inferred" | "given";

/** Where a recording came from, as its header states it (FORMAT.md, "Header"). */
export interface SourceHeader {
  readonly format: SourceFormat;
  /** File names, without directories; none for a recorder's own stream. */
  readonly files: readonly string[];
  /** The source's ticks a second, `[num, den]`: the recording's timebase is a whole multiple. */
  readonly timebase: readonly [number, number];
  /** The recording's tick where the source's tick 0 lies, before a channel's offset. */
  readonly origin: number;
  /** Ticks it was moved by against the other sources to line instruments up: a claim. */
  readonly align?: number;
  readonly instrument?: string;
  readonly software?: string;
  /** The source's own time at its tick 0, in seconds. */
  readonly begin_s?: number;
  /** The trigger's time, in the same seconds. */
  readonly trigger_s?: number;
  readonly rate?: RateOrigin;
  /** Anything else the source states. */
  readonly extra?: Readonly<Record<string, string>>;
  /** The converter's name, version and arguments. */
  readonly converter?: string;
  /** The title of the recording a merge took the source from. */
  readonly title?: string;
  /** The metadata of the recording a merge took the source from. */
  readonly meta?: Readonly<Record<string, string>>;
}

export interface RecordingInfo {
  readonly title: string;
  /** Ticks a second. */
  readonly timebase: Ratio;
  /** The capture covers ticks [0, length). */
  readonly length: number;
  readonly channels: readonly Channel[];
  /** The recording's download size in bytes, where the source knows it. */
  readonly bytes?: number;
  /** What a .tickwave header states: its content id, where its channels came from, and free
   * metadata. */
  readonly contentId?: ContentId;
  readonly sources?: readonly SourceHeader[];
  readonly meta?: Readonly<Record<string, string>>;
}

export interface Edges {
  /** The level at `start`. */
  readonly level: 0 | 1;
  /** Every edge in [start, end), in order. Two can share a tick: a pulse the source stamped with one time. */
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
  /**
   * Optional: each whole run of 64 samples' smallest and largest value, NaN left out. Run r covers
   * `values[64 r]` to `values[64 r + 63]`. A source that leaves it out costs the renderer the
   * time to build what each draw reads.
   */
  readonly runs?: SampleRuns;
}

export interface SampleRuns {
  readonly min: Float64Array | Float32Array;
  readonly max: Float64Array | Float32Array;
}

/** Samples a run of `Samples.runs` covers. */
export const SAMPLES_PER_RUN = 64;

/** Sets runs [r0, r1) of `min` and `max` from `values`. */
export function fillRuns(values: Samples["values"], min: SampleRuns["min"], max: SampleRuns["max"], r0: number, r1: number): void {
  for (let r = r0; r < r1; r++) {
    let lo = Infinity;
    let hi = -Infinity;
    for (let i = r * SAMPLES_PER_RUN; i < (r + 1) * SAMPLES_PER_RUN; i++) {
      if (values[i] < lo) lo = values[i];
      if (values[i] > hi) hi = values[i];
    }
    min[r] = lo;
    max[r] = hi;
  }
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

/** The index of the first sample whose tick is at or after `tick`, clamped to `0..=count`. */
export function sampleIndexAtOrAfter(first: number, period: Ratio, count: number, tick: number): number {
  let i = Math.ceil(((tick - first) * period.den) / period.num);
  // The estimate can land one off either way; settle it against the sample ticks themselves.
  i = Math.min(Math.max(i, 0), count);
  while (i > 0 && sampleTick(first, period, i - 1) >= tick) i--;
  while (i < count && sampleTick(first, period, i) < tick) i++;
  return i;
}

/**
 * The index of the last sample at or before `tick`: the value a cursor there reads. Null before
 * the first sample or past the channel's end.
 */
export function sampleAtOrBefore(c: AnalogChannel, tick: number): number | null {
  if (tick < c.first || tick >= c.end) return null;
  if (c.breaks?.length) {
    const i = channelIndexAtOrAfter(c, tick + 1) - 1;
    return i < 0 ? null : Math.min(i, c.count - 1);
  }
  let i = Math.floor(((tick - c.first) * c.period.den) / c.period.num);
  // The division can land one off; settle it against the sample ticks themselves.
  while (i > 0 && sampleTick(c.first, c.period, i) > tick) i--;
  while (i + 1 < c.count && sampleTick(c.first, c.period, i + 1) <= tick) i++;
  return Math.min(i, c.count - 1);
}

/**
 * An error whose `name` says what went wrong, so a page can explain it: `TickwaveNetworkError` (no
 * answer, which a refused cross-origin request looks like too), `TickwaveHttpError` (an answer that
 * was not the bytes), `TickwaveNotRecording`, `TickwaveSidecarError`, `TickwaveNoLoader` (a URL no
 * loader reads).
 */
export function named(name: string, message: string): Error {
  const e = new Error(message);
  e.name = name;
  return e;
}

/** The DOMException a source rejects with when its signal aborts. */
export function aborted(): DOMException {
  return new DOMException("aborted", "AbortError");
}

export function isAbort(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}
