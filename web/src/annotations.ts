// Annotations as the elements draw them: resolved, with times in ticks and full ids (ANNOTATIONS.md).
// A `.chrono` recording's sidecar is resolved by the Rust core (`Recording.resolveAnnotations`);
// `resolveRaw` does the same for the dev page's JSON fixtures.

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
  /** In the order the resolver gave them. */
  readonly list: readonly Annotation[];
  readonly byId: ReadonlyMap<string, Annotation>;
  /** Generated annotations a hand-written one stands in for; drawn once, as the hand-written one. */
  readonly named: ReadonlySet<string>;
  readonly errors: readonly string[];
}

const sameChannels = (a: readonly string[] | null, b: readonly string[] | null) =>
  a === b || (a !== null && b !== null && a.length === b.length && a.every((c) => b.includes(c)));

/**
 * Indexes a resolved list. The Rust resolver follows a `ref` without saying it did, so a
 * hand-written annotation with exactly a generated one's span and channels is taken to name it.
 */
export function indexAnnotations(list: readonly Annotation[], errors: readonly string[] = []): Resolved {
  const byId = new Map<string, Annotation>();
  for (const a of list) if (!byId.has(a.id)) byId.set(a.id, a);
  const named = new Set<string>();
  const generated = list.filter((a) => a.source !== null);
  for (const h of list) {
    if (h.source !== null) continue;
    if (h.ref !== null) {
      named.add(h.ref);
      continue;
    }
    for (const g of generated) {
      if (g.start === h.start && g.end === h.end && sameChannels(g.channels, h.channels)) named.add(g.id);
    }
  }
  return { list, byId, named, errors };
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

/** Resolves raw annotations: times to ticks, generated ids to full ids, `ref` followed. */
export function resolveRaw(raw: readonly RawAnnotation[], timebase: Ratio): Resolved {
  const errors: string[] = [];
  const byId = new Map<string, Annotation>();
  const add = (a: Annotation) => {
    if (byId.has(a.id)) errors.push(`annotation ${a.id} repeats`);
    else byId.set(a.id, a);
  };
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
  for (const r of raw) {
    if (r.source === undefined) continue;
    const a = base(r, `${r.source}/${r.id}`, r.source);
    if (a) add(a);
  }
  for (const r of raw) {
    if (r.source !== undefined) continue;
    if (r.id.includes("/")) {
      errors.push(`hand-written annotation ${r.id} has a "/" in its id`);
      continue;
    }
    if (r.ref === undefined) {
      const a = base(r, r.id, null);
      if (a) add(a);
      continue;
    }
    const target = byId.get(r.ref);
    if (!target || target.source === null) {
      errors.push(`annotation ${r.id} refers to ${r.ref}, which is not a generated annotation`);
      continue;
    }
    add({
      ...target,
      id: r.id,
      source: null,
      label: r.label ?? target.label,
      kind: r.kind ?? target.kind,
      text: r.text ?? target.text,
      measure: r.measure ?? target.measure,
      paint: r.paint ?? target.paint,
      ref: target.id,
    });
  }
  const list = [...byId.values()].sort((a, b) => a.start - b.start || (b.end ?? b.start) - (a.end ?? a.start));
  return indexAnnotations(list, errors);
}

/** Whether an annotation applies to a channel. */
export function covers(a: Annotation, channel: string): boolean {
  return a.channels === null || a.channels.includes(channel);
}
