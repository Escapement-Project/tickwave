// What the view measures at one point: a digital channel's pulse around a tick, with its width,
// period, frequency and duty, or where a pixel is too crowded for a pulse, its edges and its
// share of time high; and an analog channel's value or range at a pixel. The view shows it beside
// the pointer and in cursor A's readout line.

import { formatCount, formatInterval, formatRate, ONE } from "./readouts.ts";
import { counts, dutyShare } from "./render/shade.ts";
import { lowerBound } from "./search.ts";
import {
  channelSampleTick,
  channelStart,
  knownSpans,
  sampleAtOrBefore,
  type AnalogChannel,
  type AnalogOverview,
  type Channel,
  type DigitalOverview,
  type Edges,
  type Ratio,
  type Samples,
  type Source,
} from "./source.ts";

/** The ticks where a channel's level is known, `start..end`: every edge lies after `start`. */
export interface Known {
  readonly start: number;
  readonly end: number;
}

/** The span of the channel's data around `tick`, between the gaps either side; the whole of it
 * without a tick. */
export function knownSpan(c: Channel, tick?: number): Known {
  if (tick !== undefined) for (const [start, end] of knownSpans(c, -Infinity, Infinity)) if (tick >= start && tick < end) return { start, end };
  return { start: channelStart(c), end: c.end };
}

/**
 * An edge's place: a tick, `null` where there is none (the pulse runs from where the channel's level
 * is first known, or to its end), or `undefined` where the data in hand does not reach it.
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
 * the new level starts.
 */
export function pulseFromEdges(e: Edges, from: number, to: number, known: Known, tick: number): Pulse {
  const t = e.ticks;
  // The edges at or before the tick.
  const i = lowerBound(t, tick + 1);
  const level = ((e.level ^ (i & 1)) & 1) as 0 | 1;
  const start: EdgeAt = i > 0 ? t[i - 1] : from <= known.start + 1 ? null : undefined;
  const after = (k: number): EdgeAt => (k < t.length ? t[k] : to >= known.end ? null : undefined);
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
 * the overview's densities to narrow the span, a level at a time, and the edges of one small span
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
  const n = ov.density.length;
  for (let j = 0; j < n; j++) {
    const k = dir > 0 ? j : n - 1 - j;
    if (ov.density[k] === 0) continue;
    const s = Math.max(lo, ov.start + k * ov.bucket);
    const e = Math.min(hi, ov.start + (k + 1) * ov.bucket);
    if (e <= s) continue;
    // A bucket at the span's end can hold its edge outside the span: then the next one has it.
    const found = await findEdge(source, channel, s, e, dir, signal);
    if (found !== null) return found;
  }
  return null;
}

/** Fills in the edges `p` lacks, searching the source. */
export async function completePulse(source: Source, channel: number, p: Pulse, tick: number, known: Known, signal?: AbortSignal): Promise<Pulse> {
  const start = p.start !== undefined ? p.start : await findEdge(source, channel, known.start + 1, tick + 1, -1, signal);
  const end = p.end !== undefined ? p.end : await findEdge(source, channel, tick + 1, known.end, 1, signal);
  let next = p.next;
  if (next === undefined && end !== null) {
    // A pulse the source stamped with one time: the period's last edge shares the tick of `end`.
    const atEnd = (await source.edges(channel, end, end + 1, signal)).ticks.length;
    next = atEnd > 1 ? end : await findEdge(source, channel, end + 1, known.end, 1, signal);
  } else if (next === undefined) next = null;
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

/**
 * A complete pulse's figures. Widths are whole ticks; a rate and a duty carry the digits the ticks
 * support. `step` is the ticks one tick of the channel's source spans (`readingStep`).
 */
export function pulseFigures(p: Pulse, timebase: Ratio, known: Known, step: Ratio = ONE): PulseFigures {
  const level = p.level ? "high" : "low";
  const s = p.start ?? null;
  const e = p.end ?? null;
  const n = p.next ?? null;
  let width: string;
  if (s !== null && e !== null) width = formatInterval(e - s, timebase, step);
  // A channel known from the capture's start spans it; one moved or trimmed spans what it records.
  else if (s === null && e === null) width = `${known.start === 0 ? "the whole capture" : "all it records"}, ${formatInterval(known.end - known.start, timebase, step)}`;
  else width = `at least ${formatInterval(e === null ? known.end - s! : e - known.start, timebase, step)}`;
  const inverseWidth = s !== null && e !== null ? formatRate(1, e - s, timebase, 1, step) : null;
  if (s === null || e === null || n === null) return { level, width, inverseWidth, period: null, frequency: null, duty: null };
  const period = n - s;
  const high = p.level ? e - s : n - e;
  return {
    level,
    width,
    inverseWidth,
    period: formatInterval(period, timebase, step),
    frequency: formatRate(1, period, timebase, 1, step),
    duty: formatDuty(high, period, step),
  };
}

/**
 * `high` of `period` ticks, as a percentage. Each span is off by less than a tick of the source,
 * `step` ticks, so the share is uncertain by about 100 step / period percent; the last digit shown is at the largest power of ten not
 * larger than that, as a rate's is.
 */
export function formatDuty(high: number, period: number, step: Ratio = ONE): string {
  // As the view core works it: the step as one float, then the share.
  const ticks = step.num / step.den;
  const place = Math.floor(Math.log10((100 * ticks) / period));
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
  const first = sampleAtOrBefore(c, Math.max(channelStart(c), Math.ceil(t0)));
  const at = sampleAtOrBefore(c, Math.floor(tick));
  if (at === null || first === null) return { kind: "none" };
  let last = first;
  while (last + 1 < c.count && channelSampleTick(c, last + 1) < t1) last++;
  const value = (i: number) => (i >= s.first && i < s.first + s.values.length ? s.values[i - s.first] : NaN);
  if (last <= first || channelSampleTick(c, first + 1) >= t1) {
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

/**
 * A pixel too crowded to measure a pulse in: how many edges, over how many ticks, and its known
 * time's share high. From edges each is exact; from an overview each is the bounds its buckets'
 * densities and duties give (FORMAT.md, "Overview"), over the buckets the pixel meets, whole.
 */
export interface Dense {
  /** The least and the most edges, equal where exact. */
  readonly edges: readonly [number, number];
  /** The ticks the figures cover. */
  readonly ticks: number;
  /** From edges: its ticks high, of those known. */
  readonly exact?: { readonly high: number; readonly known: number };
  /** From an overview: the least and the most share of its known time high, 0 to 1. */
  readonly share?: readonly [number, number];
}

/** Ticks `[t0, t1)` from edges whose level at their span's start is `e.level`, within `known`. */
export function denseFromEdges(e: Edges, t0: number, t1: number, known: Known): Dense {
  const a = Math.max(Math.ceil(t0), known.start);
  const b = Math.max(a, Math.min(Math.ceil(t1), known.end));
  const t = e.ticks;
  const i = lowerBound(t, a);
  const j = lowerBound(t, b, i);
  let level = (e.level ^ (i & 1)) & 1;
  let at = a;
  let high = 0;
  for (let k = i; k < j; k++) {
    if (level) high += t[k] - at;
    at = t[k];
    level ^= 1;
  }
  if (level) high += b - at;
  return { edges: [j - i, j - i], ticks: b - a, exact: { high, known: b - a } };
}

/** Ticks `[t0, t1)` from an overview: the buckets they meet, whole, held to the recording's
 * `length`. A bucket outside the channel's kept span adds its edges and none of its time. */
export function denseFromOverview(o: DigitalOverview, t0: number, t1: number, length: number): Dense {
  const k0 = Math.max(0, Math.floor((t0 - o.start) / o.bucket));
  const k1 = Math.min(o.density.length, Math.max(k0 + 1, Math.ceil((t1 - o.start) / o.bucket)));
  let [lo, hi] = [0n, 0n];
  let [known, least, most] = [0, 0, 0];
  for (let k = k0; k < k1; k++) {
    const c = counts(o.density[k]);
    if (c) [lo, hi] = [lo + c[0], hi + c[1]];
    const b0 = o.start + k * o.bucket;
    const ticks = Math.max(0, Math.min(b0 + o.bucket, length) - b0);
    const s = dutyShare(o.duty[k]);
    if (!s) continue;
    known += ticks;
    least += ticks * s[1];
    most += ticks * s[2];
  }
  const start = o.start + k0 * o.bucket;
  return {
    edges: [Number(lo), Number(hi)],
    ticks: Math.max(0, Math.min(o.start + k1 * o.bucket, length) - start),
    ...(known > 0 && { share: [least / known, most / known] as const }),
  };
}

/** A share from 0 to 1 as a percentage to a tenth: the bounds a duty gives are 1/28 apart, and an
 * exact share to more would only crowd the label. */
const percent = (x: number) => `${(100 * x).toFixed(1)} %`;

/** A crowded pixel's figures as the label's lines: its edges over its time, then its share of
 * that time high, where any of it is known. `step` as `pulseFigures` takes it, for the time. */
export function denseFigures(d: Dense, timebase: Ratio, step: Ratio = ONE): string[] {
  const [lo, hi] = d.edges;
  const count = lo === hi ? formatCount(lo) : `${formatCount(lo)} to ${formatCount(hi)}`;
  const lines = [`${count} edge${hi === 1 ? "" : "s"} in ${formatInterval(d.ticks, timebase, step)}`];
  if (d.exact && d.exact.known > 0) lines.push(`high ${percent(d.exact.high / d.exact.known)}`);
  else if (d.share) {
    const [least, most] = d.share;
    if (least === most) lines.push(`high ${percent(least)}`);
    else if (least <= 0) lines.push(`high under ${percent(most)}`);
    else if (most >= 1) lines.push(`high over ${percent(least)}`);
    else lines.push(`high ${(100 * least).toFixed(1)} to ${percent(most)}`);
  }
  return lines;
}
