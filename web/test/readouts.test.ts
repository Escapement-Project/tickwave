// Readouts, from testdata/view/readouts.json, which the Rust view core's tests read too. Its
// expected values are worked by hand from the timebase and the ticks, not by running the code.
// Edges in the period rows are real: identify-swd's SWDIO and SWCLK around the first DPIDR read,
// as an independent decode of the Saleae export printed them.

import {
  clockNote,
  countEdges,
  formatBrief,
  formatClockPeriod,
  formatClockRate,
  formatCount,
  formatInterval,
  formatMarks,
  formatRate,
  formatValue,
  measureReading,
  periodStats,
  tickPlace,
  type PeriodStats,
} from "../src/readouts.ts";
import { ratio, runTable } from "./tables.ts";

/** The stats a `then` row reads: the row before's result, or its own `stats`. */
const stats = (a: { stats?: PeriodStats }, before: unknown) => (before ?? a.stats) as PeriodStats;

runTable("readouts", {
  tick_place: (a) => tickPlace(ratio(a.timebase), a.step),
  format_interval: (a) => formatInterval(a.ticks, ratio(a.timebase), a.step),
  format_brief: (a) => formatBrief(a.ticks, ratio(a.timebase), a.step),
  format_rate: (a) => formatRate(a.periods, a.span, ratio(a.timebase), a.runs, a.step),
  period_stats: (a) => periodStats(a.level, a.ticks),
  format_clock_rate: (a, { before }) => formatClockRate(stats(a, before), ratio(a.timebase), a.step),
  format_clock_period: (a, { before }) => formatClockPeriod(stats(a, before), ratio(a.timebase), a.step),
  clock_note: (a, { before }) => clockNote(stats(a, before)),
  format_value: (a) => formatValue(a.value, a.resolution, a.unit, a.significant),
  count_edges: (a) => countEdges(a.ticks, a.start, a.end),
  format_count: (a) => formatCount(a.n),
  format_marks: (a) => formatMarks(a.top, a.bottom, a.place, a.unit),
  measure_reading: (a) => measureReading(a.what, a.level, a.ticks, ratio(a.timebase)),
});
