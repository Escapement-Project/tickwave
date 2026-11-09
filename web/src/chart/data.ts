// A chart's data, `tickwave-chart/1` (CHARTS.md). `parseChart` checks a parsed JSON value and
// reports every problem it finds, not only the first.

import { SERIES_TOKEN } from "../tokens.ts";
import { isDecimal, toNumber } from "./decimal.ts";

export const FORMAT = "tickwave-chart/1";

export type Scale = "linear" | "log";
export type Pattern = "solid" | "second-build" | "reference" | "capture-second";
export type Marker = "circle" | "square" | "diamond" | "triangle-up" | "triangle-down";

export interface XAxis {
  readonly name: string | null;
  readonly unit: string | null;
  readonly scale: Scale;
  readonly values: readonly string[];
  /** The axis' ends, where they reach past the first and last settings. */
  readonly min: string | null;
  readonly max: string | null;
}

export interface YAxis {
  readonly name: string | null;
  readonly unit: string | null;
  readonly scale: Scale;
  readonly max: string | null;
  readonly step: string | null;
}

export interface Evidence {
  readonly recording: string;
  readonly annotations: string | null;
  readonly annotation: string;
}

export interface PartialRuns {
  readonly failed: number;
  readonly of: number;
  readonly value: string;
}

export interface Point {
  readonly value: string | null;
  readonly wrong: boolean;
  readonly failed: boolean;
  readonly partial: PartialRuns | null;
  readonly range: readonly [string, string] | null;
  readonly ring: boolean;
  readonly note: string | null;
  readonly evidence: Evidence | null;
}

export interface Series {
  readonly id: string;
  readonly name: string;
  readonly short: string;
  readonly colour: string;
  readonly pattern: Pattern;
  readonly marker: Marker;
  readonly capture: boolean;
  readonly hidden: boolean;
  /** Whether a failure takes a mark at the axis, or shows only in the readout. */
  readonly markFailures: boolean;
  /** One a setting; null where the series has nothing there. */
  readonly points: readonly (Point | null)[];
}

export interface Limit {
  readonly x: string;
  readonly series: string | null;
  readonly label: string;
}

export interface ChartData {
  readonly source: string | null;
  readonly x: XAxis;
  readonly y: YAxis;
  readonly series: readonly Series[];
  readonly limits: readonly Limit[];
  readonly reference: boolean;
}

const PATTERNS: readonly Pattern[] = ["solid", "second-build", "reference", "capture-second"];
const MARKERS: readonly Marker[] = ["circle", "square", "diamond", "triangle-up", "triangle-down"];
const SCALES: readonly Scale[] = ["linear", "log"];
/** A series id: `show` and `highlight` list ids by spaces and name a point as `id@x`. */
const ID = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

type Json = Record<string, unknown>;
const isObject = (v: unknown): v is Json => typeof v === "object" && v !== null && !Array.isArray(v);

/** Parses and checks chart data. Throws a `TickwaveChartError` listing every problem, one a line. */
export function parseChart(json: unknown): ChartData {
  const problems: string[] = [];
  const problem = (at: string, what: string) => problems.push(`${at}: ${what}`);
  const fail = () => {
    const e = new Error(problems.join("\n"));
    e.name = "TickwaveChartError";
    return e;
  };
  if (!isObject(json)) {
    problems.push("not a JSON object");
    throw fail();
  }
  if (json.format !== FORMAT) problem("format", `${JSON.stringify(json.format)}, not "${FORMAT}"`);

  const text = (o: Json, key: string, at: string, required = false): string | null => {
    const v = o[key];
    if (v === undefined) {
      if (required) problem(at, `no ${key}`);
      return null;
    }
    if (typeof v !== "string") {
      problem(`${at}.${key}`, "not a string");
      return null;
    }
    return v;
  };
  const decimal = (v: unknown, at: string): string | null => {
    if (isDecimal(v)) return v;
    problem(at, `${JSON.stringify(v)} is not a decimal string`);
    return null;
  };
  const oneOf = <T extends string>(v: unknown, options: readonly T[], at: string, fallback: T): T => {
    if (v === undefined) return fallback;
    if ((options as readonly unknown[]).includes(v)) return v as T;
    problem(at, `${JSON.stringify(v)} is not one of ${options.join(", ")}`);
    return fallback;
  };

  const xIn = isObject(json.x) ? json.x : (problem("x", "not an object"), {});
  const yIn = isObject(json.y) ? json.y : (problem("y", "not an object"), {});
  const xScale = oneOf(xIn.scale, SCALES, "x.scale", "linear");
  const yScale = oneOf(yIn.scale, SCALES, "y.scale", "linear");
  const values: string[] = [];
  if (!Array.isArray(xIn.values) || xIn.values.length === 0) problem("x.values", "not a list of settings");
  else
    xIn.values.forEach((v, i) => {
      const d = decimal(v, `x.values[${i}]`);
      if (d === null) return;
      if (values.length && toNumber(d) <= toNumber(values[values.length - 1])) problem(`x.values[${i}]`, "not above the one before");
      if (xScale === "log" && toNumber(d) <= 0) problem(`x.values[${i}]`, "not above 0 on a log scale");
      values.push(d);
    });
  const xMin = xIn.min === undefined ? null : decimal(xIn.min, "x.min");
  const xMax = xIn.max === undefined ? null : decimal(xIn.max, "x.max");
  if (xMin !== null && values.length && toNumber(xMin) > toNumber(values[0])) problem("x.min", "above the first setting");
  if (xMax !== null && values.length && toNumber(xMax) < toNumber(values[values.length - 1])) problem("x.max", "below the last setting");
  if (xMin !== null && xScale === "log" && toNumber(xMin) <= 0) problem("x.min", "not above 0 on a log scale");
  const yMax = yIn.max === undefined ? null : decimal(yIn.max, "y.max");
  const yStep = yIn.step === undefined ? null : decimal(yIn.step, "y.step");
  if (yStep !== null && toNumber(yStep) <= 0) problem("y.step", "not above 0");

  const ids = new Set<string>();
  const series: Series[] = [];
  if (!Array.isArray(json.series)) problem("series", "not a list");
  else
    json.series.forEach((s, n) => {
      const at = `series[${n}]`;
      if (!isObject(s)) return problem(at, "not an object");
      const id = text(s, "id", at, true) ?? `#${n}`;
      if (s.id !== undefined && !ID.test(id)) problem(`${at}.id`, `${JSON.stringify(id)} is not letters, digits, ".", "_" and "-"`);
      if (ids.has(id)) problem(at, `id ${JSON.stringify(id)} is used twice`);
      ids.add(id);
      const name = text(s, "name", at, true) ?? id;
      const colour = text(s, "colour", at, true) ?? "";
      if (colour && !SERIES_TOKEN.test(colour)) problem(`${at}.colour`, `${JSON.stringify(colour)} is not a series token name`);
      const fails = s.fails;
      if (fails !== undefined && fails !== "mark") problem(`${at}.fails`, `${JSON.stringify(fails)} is not "mark"`);
      const points: (Point | null)[] = [];
      if (!Array.isArray(s.points) || s.points.length !== values.length) {
        problem(`${at}.points`, `not one a setting (${values.length})`);
      } else {
        s.points.forEach((p, i) => points.push(parsePoint(p, `${at}.points[${i}]`, yScale)));
      }
      series.push({
        id,
        name,
        short: text(s, "short", at) ?? name,
        colour,
        pattern: oneOf(s.pattern, PATTERNS, `${at}.pattern`, "solid"),
        marker: oneOf(s.marker, MARKERS, `${at}.marker`, "circle"),
        capture: s.capture === true,
        hidden: s.hidden === true,
        markFailures: fails === "mark",
        points,
      });
    });

  function parsePoint(p: unknown, at: string, scale: Scale): Point | null {
    if (p === null) return null;
    if (!isObject(p)) {
      problem(at, "not an object or null");
      return null;
    }
    const value = p.value === undefined ? null : decimal(p.value, `${at}.value`);
    if (value !== null && scale === "log" && toNumber(value) <= 0) problem(`${at}.value`, "not above 0 on a log scale");
    if (p.verdict !== undefined && p.verdict !== "wrong") problem(`${at}.verdict`, `${JSON.stringify(p.verdict)} is not "wrong"`);
    let partial: PartialRuns | null = null;
    if (p.partial !== undefined) {
      const q = p.partial;
      if (!isObject(q) || !Number.isInteger(q.failed) || !Number.isInteger(q.of) || !((q.failed as number) >= 1 && (q.failed as number) < (q.of as number))) {
        problem(`${at}.partial`, "not { failed, of, value } with 1 <= failed < of");
      } else {
        const v = decimal(q.value, `${at}.partial.value`);
        if (v !== null) partial = { failed: q.failed as number, of: q.of as number, value: v };
      }
      if (value !== null) problem(at, "a partial point has no value: the line skips it");
    }
    let range: [string, string] | null = null;
    if (p.range !== undefined) {
      if (!Array.isArray(p.range) || p.range.length !== 2) problem(`${at}.range`, "not [low, high]");
      else {
        const lo = decimal(p.range[0], `${at}.range[0]`);
        const hi = decimal(p.range[1], `${at}.range[1]`);
        if (lo !== null && hi !== null) range = [lo, hi];
      }
    }
    let evidence: Evidence | null = null;
    if (p.evidence !== undefined) {
      const e = p.evidence;
      if (!isObject(e)) problem(`${at}.evidence`, "not an object");
      else {
        const recording = text(e, "recording", `${at}.evidence`, true);
        const annotation = text(e, "annotation", `${at}.evidence`, true);
        if (recording !== null && annotation !== null) {
          evidence = { recording, annotation, annotations: text(e, "annotations", `${at}.evidence`) };
        }
      }
    }
    return {
      value,
      wrong: p.verdict === "wrong",
      failed: p.failed === true,
      partial,
      range,
      ring: p.ring === true,
      note: text(p, "note", at),
      evidence,
    };
  }

  const limits: Limit[] = [];
  if (json.limits !== undefined) {
    if (!Array.isArray(json.limits)) problem("limits", "not a list");
    else
      json.limits.forEach((l, n) => {
        const at = `limits[${n}]`;
        if (!isObject(l)) return problem(at, "not an object");
        const x = decimal(l.x, `${at}.x`);
        const label = text(l, "label", at, true);
        const of = text(l, "series", at);
        if (of !== null && !ids.has(of)) problem(`${at}.series`, `no series ${JSON.stringify(of)}`);
        if (x !== null && label !== null) limits.push({ x, label, series: of });
      });
  }
  if (json.reference !== undefined && json.reference !== "y=x") problem("reference", `${JSON.stringify(json.reference)} is not "y=x"`);

  if (problems.length) throw fail();
  return {
    source: typeof json.source === "string" ? json.source : null,
    x: { name: text(xIn, "name", "x"), unit: text(xIn, "unit", "x"), scale: xScale, values, min: xMin, max: xMax },
    y: { name: text(yIn, "name", "y"), unit: text(yIn, "unit", "y"), scale: yScale, max: yMax, step: yStep },
    series,
    limits,
    reference: json.reference === "y=x",
  };
}
