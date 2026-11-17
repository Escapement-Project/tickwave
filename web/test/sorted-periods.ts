// The period figures as `periodStats` worked them out before it took edges one at a time: every
// period kept and sorted, whatever their count. The tests hold the counter to it up to
// `DISTINCT_AT_MOST` distinct periods.

import type { PeriodStats } from "../src/readouts.ts";

export function sortedStats(level: 0 | 1, ticks: ArrayLike<number>): PeriodStats | null {
  const first = level === 0 ? 0 : 1;
  const count = Math.max(0, Math.floor((ticks.length - first - 1) / 2));
  if (count === 0) return null;
  const periods = new Float64Array(count);
  for (let k = 0; k < count; k++) periods[k] = ticks[first + 2 * k + 2] - ticks[first + 2 * k];
  const sorted = periods.slice().sort();
  const median = sorted[(count - 1) >> 1];
  const q1 = sorted[Math.floor((count - 1) / 4)];
  const q3 = sorted[Math.ceil((3 * (count - 1)) / 4)];
  const iqr = Math.max(q3 - q1, 1);
  const [lo, hi] = [q1 - 1.5 * iqr, q3 + 1.5 * iqr];
  let used = 0;
  let runs = 0;
  let span = 0;
  let inRun = false;
  for (const p of periods) {
    if (p >= lo && p <= hi) {
      used++;
      span += p;
      if (!inRun) runs++;
      inRun = true;
    } else inRun = false;
  }
  return { count, median, min: sorted[0], max: sorted[count - 1], used, runs, span };
}
