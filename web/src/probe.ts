// What the view measures at one point: a digital channel's pulse around a tick, with its width,
// period, frequency and duty, and an analog channel's value or range at a pixel. The view shows it
// beside the pointer and in cursor A's readout line.

import { formatInterval, formatRate } from "./readouts.ts";
import { lowerBound } from "./search.ts";
import { sampleAtOrBefore, sampleTick, type AnalogChannel, type AnalogOverview, type Edges, type Ratio, type Samples, type Source } from "./source.ts";

/**
 * An edge's place: a tick, `null` where there is none (the pulse runs from the capture's start or
 * to the channel's end), or `undefined` where the data in hand does not reach it.
 */
export type EdgeAt = number | null | undefined;

/** The pulse a tick lies in: the level over it and the edges around it. */
export interface Pulse {
  readonly level: 0 | 1;
  /** The edge the pulse starts at. */
  readonly start: EdgeAt;
  /** The edge it ends at. */
  readonly end: EdgeAt;
  /** The edge after `end`, which closes the period that starts at `start`. */
  readonly next: EdgeAt;
}

/**
 * The pulse at `tick` from edges read over [from, to), which hold `tick`. An edge at a tick is where
 * the new level starts. `known` is the channel's end: an edge can lie at it, none after it.
 */
export function pulseFromEdges(e: Edges, from: number, to: number, known: number, tick: number): Pulse {
  const t = e.ticks;
  // The edges at or before the tick.
  const i = lowerBound(t, tick + 1);
  const level = ((e.level ^ (i & 1)) & 1) as 0 | 1;
  const start: EdgeAt = i > 0 ? t[i - 1] : from <= 1 ? null : undefined;
  const after = (k: number): EdgeAt => (k < t.length ? t[k] : to > known ? null : undefined);
  const end = after(i);
  return { level, start, end, next: end === null ? null : after(i + 1) };
}

/** Whether every edge the pulse's figures need is in hand. */
export function complete(p: Pulse): boolean {
  return p.start !== undefined && p.end !== undefined && p.next !== undefined;
}

/** Below this many ticks, a search reads the span's edges rather than narrowing further. */
const DIRECT = 1 << 16;
/** A search narrows by about this many buckets a step. */
const FAN = 64;

/**
 * The first edge in [lo, hi) (`dir` 1) or the last (`dir` -1), or null where there is none. It reads
 * the overview's change bits to narrow the span, a level at a time, and the edges of one small span
 * at the end, so finding the edges of a long quiet stretch downloads little.
 */
export async function findEdge(source: Source, channel: number, lo: number, hi: number, dir: 1 | -1, signal?: AbortSignal): Promise<number | null> {
  if (hi <= lo) return null;
  const direct = async () => {
    const t = (await source.edges(channel, lo, hi, signal)).ticks;
    const k = dir > 0 ? lowerBound(t, lo) : lowerBound(t, hi) - 1;
    return k >= 0 && k < t.length && t[k] >= lo && t[k] < hi ? t[k] : null;
  };
  if (hi - lo <= DIRECT) return direct();
  const ov = await source.overview(channel, lo, hi, Math.ceil((hi - lo) / FAN), signal);
  if (!ov || ov.kind !== "digital" || ov.bucket >= hi - lo) return direct();
  const n = ov.changes.length;
  for (let j = 0; j < n; j++) {
    const k = dir > 0 ? j : n - 1 - j;
    if (!ov.changes[k]) continue;
    const s = Math.max(lo, ov.start + k * ov.bucket);
    const e = Math.min(hi, ov.start + (k + 1) * ov.bucket);
    if (e <= s) continue;
    // A bucket at the span's end can hold its edge outside the span: then the next one has it.
    const found = await findEdge(source, channel, s, e, dir, signal);
    if (found !== null) return found;
  }
  return null;
}

/** Fills in the edges `p` lacks, searching the source. `known` is the channel's end, as above. */
export async function completePulse(source: Source, channel: number, p: Pulse, tick: number, known: number, signal?: AbortSignal): Promise<Pulse> {
  const start = p.start !== undefined ? p.start : await findEdge(source, channel, 1, tick + 1, -1, signal);
  const end = p.end !== undefined ? p.end : await findEdge(source, channel, tick + 1, known + 1, 1, signal);
  let next = p.next;
  if (next === undefined) next = end === null ? null : await findEdge(source, channel, end + 1, known + 1, 1, signal);
  return { level: p.level, start, end, next };
}

/** A pulse's figures, as text: what a label or a readout line shows. */
export interface PulseFigures {
  readonly level: "high" | "low";
  /** The pulse's width; "at least ..." where it runs from the capture's start or to its end. */
  readonly width: string;
  /** The width's reciprocal as a rate, where both edges are known: one UART bit gives the baud rate. */
  readonly inverseWidth: string | null;
  readonly period: string | null;
  readonly frequency: string | null;
  /** The share of the period spent high. */
  readonly duty: string | null;
}

/** A complete pulse's figures. Widths are whole ticks; a rate and a duty carry the digits the ticks support. */
export function pulseFigures(p: Pulse, timebase: Ratio, known: number): PulseFigures {
  const level = p.level ? "high" : "low";
  const s = p.start ?? null;
  const e = p.end ?? null;
  const n = p.next ?? null;
  let width: string;
  if (s !== null && e !== null) width = formatInterval(e - s, timebase);
  else if (s === null && e === null) width = `the whole capture, ${formatInterval(known, timebase)}`;
  else width = `at least ${formatInterval(e === null ? known - s! : e, timebase)}`;
  const inverseWidth = s !== null && e !== null ? formatRate(1, e - s, timebase) : null;
  if (s === null || e === null || n === null) return { level, width, inverseWidth, period: null, frequency: null, duty: null };
  const period = n - s;
  const high = p.level ? e - s : n - e;
  return {
    level,
    width,
    inverseWidth,
    period: formatInterval(period, timebase),
    frequency: formatRate(1, period, timebase),
    duty: formatDuty(high, period),
  };
}

/**
 * `high` of `period` ticks, as a percentage. Each span is off by less than a tick, so the share is
 * uncertain by about 100 / period percent; the last digit shown is at the largest power of ten not
 * larger than that, as a rate's is.
 */
export function formatDuty(high: number, period: number): string {
  const place = Math.floor(Math.log10(100 / period));
  const share = (100 * high) / period;
  // A no-break space before the unit, as every readout writes it.
  if (place < 0) return `${share.toFixed(-place)}\u00a0%`;
  return `${Math.round(share / 10 ** place) * 10 ** place}\u00a0%`;
}

/** An analog channel at one pixel of the plot. */
export type AnalogAt =
  | { readonly kind: "value"; readonly value: number }
  | { readonly kind: "range"; readonly min: number; readonly max: number }
  | { readonly kind: "none" };

/**
 * An analog channel over the ticks [t0, t1) a pixel covers, from samples read over the span: the
 * one sample at or before `tick` where the pixel holds at most one, else the smallest and largest
 * in it, as the trace draws them.
 */
export function analogFromSamples(c: AnalogChannel, s: Samples, t0: number, t1: number, tick: number): AnalogAt {
  const first = sampleAtOrBefore(c, Math.max(c.first, Math.ceil(t0)));
  const at = sampleAtOrBefore(c, Math.floor(tick));
  if (at === null || first === null) return { kind: "none" };
  let last = first;
  while (last + 1 < c.count && sampleTick(c.first, c.period, last + 1) < t1) last++;
  const value = (i: number) => (i >= s.first && i < s.first + s.values.length ? s.values[i - s.first] : NaN);
  if (last <= first || sampleTick(c.first, c.period, first + 1) >= t1) {
    const v = value(at);
    return Number.isFinite(v) ? { kind: "value", value: v } : { kind: "none" };
  }
  let min = Infinity;
  let max = -Infinity;
  for (let i = first; i <= last; i++) {
    const v = value(i);
    if (!Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
  }
  return min <= max ? (min === max ? { kind: "value", value: min } : { kind: "range", min, max }) : { kind: "none" };
}

/** An analog channel over [t0, t1) from an overview: the smallest and largest of the buckets it meets. */
export function analogFromOverview(o: AnalogOverview, t0: number, t1: number): AnalogAt {
  const k0 = Math.max(0, Math.floor((t0 - o.start) / o.bucket));
  const k1 = Math.min(o.min.length, Math.ceil((t1 - o.start) / o.bucket));
  let min = Infinity;
  let max = -Infinity;
  for (let k = k0; k < k1; k++) {
    if (o.min[k] < min) min = o.min[k];
    if (o.max[k] > max) max = o.max[k];
  }
  return min <= max ? (min === max ? { kind: "value", value: min } : { kind: "range", min, max }) : { kind: "none" };
}
