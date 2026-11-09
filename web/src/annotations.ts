// Annotations as the elements draw them: resolved, with times in ticks and full ids, in sidecar
// order (ANNOTATIONS.md). A `.tickwave` recording's sidecar is resolved by the Rust core
// (`Recording.resolveAnnotations`); `resolveRaw` does the same for the dev page's JSON fixtures.

import type { Ratio } from "./source.ts";
import { parseTime, toTicks } from "./time.ts";

export interface Measure {
  readonly channel: string;
  readonly what: string;
}

/** An annotation with the sidecar's fields, as a generator or a hand-written file states it. */
export interface RawAnnotation {
  readonly id: string;
  readonly start?: number | string;
  readonly end?: number | string;
  readonly channels?: readonly string[];
  readonly label?: string;
  readonly kind?: string;
  readonly text?: string;
  readonly measure?: Measure;
  readonly paint?: string;
  readonly ref?: string;
  /** The generator that wrote it. Absent on a hand-written annotation. */
  readonly source?: string;
}

export interface Annotation {
  /** "<source>/<id>" for a generated annotation; a hand-written id has no "/". */
  readonly id: string;
  /** The generator's name; null for a hand-written annotation. */
  readonly source: string | null;
  readonly start: number;
  /** Null for a point. Exclusive. */
  readonly end: number | null;
  /** Null: every channel. */
  readonly channels: readonly string[] | null;
  readonly label: string | null;
  readonly kind: string | null;
  /** Longer text, for an inset's caption. */
  readonly text: string | null;
  readonly measure: Measure | null;
  readonly paint: string | null;
  /** The generated annotation this hand-written one names, where the resolver says. */
  readonly ref: string | null;
}

export interface Resolved {
  /** In sidecar order: the hand-written file's, then each included file's. The order carries meaning. */
  readonly list: readonly Annotation[];
  /** The same, by start, then longer first. */
  readonly byTime: readonly Annotation[];
  readonly byId: ReadonlyMap<string, Annotation>;
  /** Generated annotations a hand-written one stands in for; drawn once, as the hand-written one. */
  readonly named: ReadonlySet<string>;
  readonly errors: readonly string[];
}

/** Indexes a resolved list. An alias's `ref` names the generated annotation it stands in for. */
export function indexAnnotations(list: readonly Annotation[], errors: readonly string[] = []): Resolved {
  const byId = new Map<string, Annotation>();
  for (const a of list) if (!byId.has(a.id)) byId.set(a.id, a);
  const named = new Set<string>();
  for (const a of list) if (a.ref !== null) named.add(a.ref);
  const byTime = [...list].sort((a, b) => a.start - b.start || (b.end ?? b.start) - (a.end ?? a.start));
  return { list, byTime, byId, named, errors };
}

/** One annotation as the Rust resolver serialises it. */
export interface CoreAnnotation {
  id: string;
  source: string | null;
  start: number;
  end: number | null;
  channels: string[];
  label: string | null;
  kind: string | null;
  text: string | null;
  paint: string | null;
  measure: Measure | null;
  ref?: string | null;
}

export function fromCore(a: CoreAnnotation): Annotation {
  return {
    id: a.id,
    source: a.source,
    start: a.start,
    end: a.end,
    channels: a.channels.length > 0 ? a.channels : null,
    label: a.label,
    kind: a.kind,
    text: a.text,
    measure: a.measure,
    paint: a.paint,
    ref: a.ref ?? null,
  };
}

function time(v: number | string | undefined, timebase: Ratio): number | null | undefined {
  if (v === undefined) return undefined;
  if (typeof v === "number") return Number.isSafeInteger(v) ? v : null;
  const spec = parseTime(v);
  return spec ? toTicks(spec, timebase) : null;
}

/**
 * Resolves raw annotations: times to ticks, generated ids to full ids, `ref` followed. The result
 * keeps the input's order, as the Rust resolver keeps sidecar order.
 */
export function resolveRaw(raw: readonly RawAnnotation[], timebase: Ratio): Resolved {
  const errors: string[] = [];
  const seen = new Set<string>();
  const generated = new Map<string, Annotation>();
  const base = (r: RawAnnotation, id: string, source: string | null): Annotation | null => {
    const start = time(r.start, timebase);
    const end = time(r.end, timebase);
    if (start === null || start === undefined || end === null) {
      errors.push(`annotation ${id} has no readable start or end`);
      return null;
    }
    if (end !== undefined && end < start) {
      errors.push(`annotation ${id} ends before it starts`);
      return null;
    }
    return {
      id,
      source,
      start,
      end: end ?? null,
      channels: r.channels ?? null,
      label: r.label ?? null,
      kind: r.kind ?? null,
      text: r.text ?? null,
      measure: r.measure ?? null,
      paint: r.paint ?? null,
      ref: null,
    };
  };
  // Generated annotations first, so a hand-written ref can find its target wherever it sits.
  for (const r of raw) {
    if (r.source === undefined) continue;
    const a = base(r, `${r.source}/${r.id}`, r.source);
    if (a && !generated.has(a.id)) generated.set(a.id, a);
  }
  const list: Annotation[] = [];
  for (const r of raw) {
    let a: Annotation | null = null;
    if (r.source !== undefined) a = generated.get(`${r.source}/${r.id}`) ?? null;
    else if (r.id.includes("/")) errors.push(`hand-written annotation ${r.id} has a "/" in its id`);
    else if (r.ref === undefined) a = base(r, r.id, null);
    else {
      const target = generated.get(r.ref);
      if (!target) errors.push(`annotation ${r.id} refers to ${r.ref}, which is not a generated annotation`);
      else
        a = {
          ...target,
          id: r.id,
          source: null,
          label: r.label ?? target.label,
          kind: r.kind ?? target.kind,
          text: r.text ?? target.text,
          measure: r.measure ?? target.measure,
          paint: r.paint ?? target.paint,
          ref: target.id,
        };
    }
    if (!a) continue;
    if (seen.has(a.id)) {
      errors.push(`annotation ${a.id} repeats`);
      continue;
    }
    seen.add(a.id);
    list.push(a);
  }
  return indexAnnotations(list, errors);
}

/**
 * The annotations a lane draws, in time order: those with a label, or neither a label nor a paint
 * role, on one of `channels`, other than generated ones an alias stands in for; only `only`'s
 * when given.
 */
export function laneAnnotations(r: Resolved, channels: ReadonlySet<string>, only: ReadonlySet<string> | null): Annotation[] {
  return r.byTime.filter(
    (a) =>
      (only === null || only.has(a.id)) &&
      !r.named.has(a.id) &&
      (a.label !== null || a.paint === null) &&
      (a.channels === null || a.channels.some((c) => channels.has(c))),
  );
}

/** The annotations of one kind, in time order, to step through. */
export function ofKind(r: Resolved, kind: string): Annotation[] {
  return r.byTime.filter((a) => a.kind === kind && !r.named.has(a.id));
}

/** Whether an annotation applies to a channel. */
export function covers(a: Annotation, channel: string): boolean {
  return a.channels === null || a.channels.includes(channel);
}
