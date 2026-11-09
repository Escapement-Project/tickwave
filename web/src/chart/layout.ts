// A chart's geometry from its data and its box: where every point, tick, label and mark goes. Pure,
// so the element only draws what this decides.

import { scaleLinear, scaleLog } from "d3-scale";
import type { ChartData, Point, Series } from "./data.ts";
import { grouped, toNumber } from "./decimal.ts";

/** The plot's margins, as the landing page's Figure 1 has them. */
const LEFT = 52;
const TOP = 28;
const BOTTOM = 44;
/** Room on the right for the end labels, or only for a marker without them. */
const RIGHT_LABELS = 64;
const RIGHT_BARE = 18;
/** An x label closer than this to the one before it is left out. */
const LABEL_GAP = 22;

export interface Placed {
  readonly index: number;
  readonly x: number;
  readonly y: number;
  readonly point: Point;
}

export interface SeriesLayout {
  readonly series: Series;
  /** The points with a value, in x order. */
  readonly placed: readonly Placed[];
  /** Placed points at neighbouring settings; `wrong` when the point it runs into reads wrong. */
  readonly segments: readonly { readonly from: Placed; readonly to: Placed; readonly wrong: boolean }[];
}

export interface Layout {
  readonly width: number;
  readonly height: number;
  /** The plot's edges. */
  readonly left: number;
  readonly right: number;
  readonly top: number;
  readonly bottom: number;
  /** Each setting's x. */
  readonly xs: readonly number[];
  /** The settings labelled, after thinning. */
  readonly xLabels: readonly { readonly x: number; readonly text: string }[];
  readonly yTicks: readonly { readonly y: number; readonly text: string }[];
  /** For each setting, the span of x that picks it: halfway to each neighbour. */
  readonly bands: readonly (readonly [number, number])[];
  readonly series: readonly SeriesLayout[];
  readonly fails: readonly { readonly x: number; readonly y: number; readonly text: string; readonly series: string }[];
  readonly limits: readonly { readonly x: number; readonly label: string; readonly origin: number | null; readonly reach: number | null }[];
  readonly reference: string | null;
  /** The sweep runs from just before the first setting's markers to just past the last's. */
  readonly sweepFrom: number;
  readonly sweepTo: number;
  y(value: number): number;
}

export function layout(data: ChartData, shown: ReadonlySet<string>, width: number, height: number, endLabels: boolean): Layout {
  const left = LEFT;
  const right = width - (endLabels ? RIGHT_LABELS : RIGHT_BARE);
  const top = TOP;
  const bottom = height - BOTTOM;
  const xValues = data.x.values.map(toNumber);
  const xDomain = [xValues[0], xValues[xValues.length - 1]];
  const xScale = (data.x.scale === "log" ? scaleLog() : scaleLinear()).domain(xDomain).range([left, right]);
  // One setting has no extent: put it in the middle.
  const xs = xValues.length === 1 ? [(left + right) / 2] : xValues.map((v) => xScale(v));

  const drawn = data.series.filter((s) => shown.has(s.id));
  const values = drawn.flatMap((s) => s.points.flatMap((p) => (p?.value != null ? [toNumber(p.value)] : [])));
  if (data.reference) values.push(...xValues);
  const { scale: yScale, ticks } = yAxis(data, values);
  yScale.range([bottom, top]);
  const y = (v: number) => yScale(v);

  const xLabels: { x: number; text: string }[] = [];
  let last = -Infinity;
  data.x.values.forEach((v, i) => {
    if (xs[i] - last < LABEL_GAP) return;
    xLabels.push({ x: xs[i], text: grouped(v) });
    last = xs[i];
  });

  const bands = xs.map((x, i) => [i === 0 ? left : (xs[i - 1] + x) / 2, i === xs.length - 1 ? right : (x + xs[i + 1]) / 2] as const);

  const series = drawn.map((s): SeriesLayout => {
    const placed: Placed[] = [];
    s.points.forEach((point, index) => {
      if (point?.value != null) placed.push({ index, x: xs[index], y: y(toNumber(point.value)), point });
    });
    // A segment joins neighbouring settings that both have a value. One across a setting without
    // a value, failed, partly failed or never measured, would read as a figure there.
    const segments = placed.slice(1).flatMap((to, i) => {
      const from = placed[i];
      return to.index === from.index + 1 ? [{ from, to, wrong: to.point.wrong }] : [];
    });
    return { series: s, placed, segments };
  });

  // A failure's mark sits at the axis, a row a series, so two at one setting do not overlap.
  const fails: Layout["fails"][number][] = [];
  drawn
    .filter((s) => s.markFailures)
    .forEach((s, row) =>
      s.points.forEach((p, i) => {
        if (p && p.value == null && (p.failed || p.partial)) {
          fails.push({ x: xs[i], y: bottom - 10 - 16 * row, text: `${s.short} failed`, series: s.id });
        }
      }),
    );

  const limits = data.limits.map((l) => {
    const x = data.x.values.length === 1 ? xs[0] : xScale(toNumber(l.x));
    const of = series.find((s) => s.series.id === l.series);
    const at = of?.placed.find((p) => data.x.values[p.index] === l.x);
    // The limit grows from its series' point as the sweep crosses from there to the series' end.
    const end = of?.placed[of.placed.length - 1];
    return { x, label: l.label, origin: at ? at.y : null, reach: at && end && end.x > at.x ? end.x : null };
  });

  const reference = data.reference
    ? xs.map((x, i) => `${i ? "L" : "M"}${f1(x)} ${f1(y(xValues[i]))}`).join("")
    : null;

  return {
    width,
    height,
    left,
    right,
    top,
    bottom,
    xs,
    xLabels,
    yTicks: ticks.map((v) => ({ y: y(v), text: grouped(plain(v)) })),
    bands,
    series,
    fails,
    limits,
    reference,
    sweepFrom: xs[0] - 6,
    sweepTo: xs[xs.length - 1] + 6,
    y,
  };
}

/** The y scale and its ticks: fixed by `y.max` and `y.step`, else d3's steps of 1, 2 or 5. */
function yAxis(data: ChartData, values: number[]) {
  if (data.y.scale === "log") {
    const lo = Math.min(...values, Infinity);
    const hi = Math.max(...values, -Infinity);
    const domain = Number.isFinite(lo) ? [lo * 0.8, hi * 1.25] : [1, 10];
    const scale = scaleLog().domain(domain);
    const ticks: number[] = [];
    for (let e = Math.floor(Math.log10(domain[0])); e <= Math.ceil(Math.log10(domain[1])); e++) {
      for (const m of [1, 2, 5]) {
        const v = m * 10 ** e;
        if (v >= domain[0] && v <= domain[1]) ticks.push(Number(v.toPrecision(12)));
      }
    }
    return { scale, ticks };
  }
  const step = data.y.step ? toNumber(data.y.step) : null;
  const top = data.y.max ? toNumber(data.y.max) : Math.max(...values, 0) * 1.04 || 1;
  if (step) {
    const max = data.y.max ? top : Math.ceil(top / step) * step;
    const ticks = Array.from({ length: Math.floor(max / step + 1e-9) + 1 }, (_, i) => Number((i * step).toPrecision(12)));
    return { scale: scaleLinear().domain([0, max]), ticks };
  }
  const scale = scaleLinear().domain([0, top]);
  if (!data.y.max) scale.nice(4);
  return { scale, ticks: scale.ticks(4) };
}

/** A tick's value as plain decimal digits, for grouping. */
function plain(v: number): string {
  return Number(v.toPrecision(12)).toString();
}

export const f1 = (v: number): string => (Math.round(v * 10) / 10).toString();
