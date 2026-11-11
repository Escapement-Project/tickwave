// An analog row's value range, as the reader zooms and moves it: the arithmetic, apart from the
// pointer and the canvas.

import { niceStep } from "../readouts.ts";

export interface ValueRange {
  readonly lo: number;
  readonly hi: number;
}

/** How far a channel's range may zoom and where it may sit. `whole` is its range unzoomed. */
export interface ScaleLimits {
  readonly whole: ValueRange;
  readonly minSpan: number;
  readonly maxSpan: number;
}

/** In to four steps of the instrument's resolution, where it has one, and out to four times the whole. */
export function scaleLimits(whole: ValueRange, resolution: number | null): ScaleLimits {
  const span = whole.hi - whole.lo;
  const minSpan = resolution !== null && resolution > 0 ? Math.max(4 * resolution, span * 1e-6) : span * 1e-4;
  return { whole, minSpan: Math.min(minSpan, span), maxSpan: 4 * span };
}

/** `r` with its span within the limits, about its middle, and its middle within the whole range. */
export function clampRange(r: ValueRange, lim: ScaleLimits): ValueRange {
  const span = Math.min(lim.maxSpan, Math.max(lim.minSpan, r.hi - r.lo));
  const mid = Math.min(lim.whole.hi, Math.max(lim.whole.lo, (r.lo + r.hi) / 2));
  return { lo: mid - span / 2, hi: mid + span / 2 };
}

/** `r` zoomed by `factor` (above 1 zooms out) about the value `at`, which keeps its place. */
export function zoomRange(r: ValueRange, at: number, factor: number, lim: ScaleLimits): ValueRange {
  const span = Math.min(lim.maxSpan, Math.max(lim.minSpan, (r.hi - r.lo) * factor));
  const f = (at - r.lo) / (r.hi - r.lo);
  return clampRange({ lo: at - f * span, hi: at - f * span + span }, lim);
}

/** `r` moved by `by` in value. */
export function moveRange(r: ValueRange, by: number, lim: ScaleLimits): ValueRange {
  return clampRange({ lo: r.lo + by, hi: r.hi + by }, lim);
}

/** The range that just holds the values from `min` to `max`, or null for none. */
export function fitRange(min: number, max: number, lim: ScaleLimits): ValueRange | null {
  if (!(min <= max)) return null;
  return clampRange({ lo: min, hi: max }, lim);
}

/**
 * Where an analog row's two scale labels go: the highest and lowest multiples of a round step
 * inside `r`, and the decimal place they carry. The step is at most a quarter of the range, so the
 * labels stay near its edges, clear of the channel's name between them.
 */
export function rangeMarks(r: ValueRange): { top: number; bottom: number; place: number } {
  const { step, place } = niceStep((r.hi - r.lo) / 10);
  // 3.6 / 0.2 is 17.999999999999996 in binary: a value on a step counts as on it.
  return { top: Math.floor(r.hi / step + 1e-9) * step, bottom: Math.ceil(r.lo / step - 1e-9) * step, place };
}
