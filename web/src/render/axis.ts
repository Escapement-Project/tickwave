// The time axis: labelled ticks at 1, 2 or 5 times a power of ten seconds, never closer than one
// tick of the recording, spaced so no two labels touch. Every label shares one unit.

import { formatScaled, TIME_UNITS, unitFor, type Unit } from "../readouts.ts";
import type { Ratio } from "../source.ts";

export interface AxisTick {
  readonly tick: number;
  /** From the left of the plot, in CSS pixels. */
  readonly x: number;
  readonly label: string;
  readonly width: number;
}

export interface Axis {
  readonly ticks: readonly AxisTick[];
  readonly unit: Unit | null;
}

const MULTIPLES = [1, 2, 5];

/**
 * Ticks for the view [start, end) over `width` CSS pixels. A label is kept only where it fits
 * inside the plot whole.
 */
export function timeAxis(
  start: number,
  end: number,
  width: number,
  timebase: Ratio,
  measure: (label: string) => number,
  gap = 16,
): Axis {
  const span = end - start;
  if (!(span > 0) || !(width > 0)) return { ticks: [], unit: null };
  const perPx = span / width;
  const tickSeconds = timebase.den / timebase.num;
  // Start a decade below the smaller of one tick and 24 pixels' worth of time.
  let e = Math.floor(Math.log10(Math.min(tickSeconds, (24 * perPx) * tickSeconds))) - 1;
  for (let guard = 0; guard < 40; guard++, e++) {
    for (const s of MULTIPLES) {
      const stepSeconds = s * 10 ** e;
      if (stepSeconds < tickSeconds * (1 - 1e-12)) continue;
      const stepTicks = stepSeconds / tickSeconds;
      const stepPx = stepTicks / perPx;
      if (stepPx < 24) continue;
      const m0 = Math.ceil(start / stepTicks - 1e-9);
      const m1 = Math.floor(end / stepTicks + 1e-9);
      if (m1 - m0 > 400) continue;
      const biggest = Math.max(Math.abs(m0), Math.abs(m1)) * s;
      const unit = unitFor(BigInt(biggest), e, TIME_UNITS);
      const ticks: AxisTick[] = [];
      let widest = 0;
      for (let m = m0; m <= m1; m++) {
        const label = formatScaled(BigInt(m * s), e, TIME_UNITS, unit);
        const w = measure(label);
        widest = Math.max(widest, w);
        const tick = m * stepTicks;
        ticks.push({ tick, x: (tick - start) / perPx, label, width: w });
      }
      if (stepPx < widest + gap) continue;
      return { ticks: ticks.filter((t) => t.x - t.width / 2 >= 0 && t.x + t.width / 2 <= width), unit };
    }
  }
  return { ticks: [], unit: null };
}
