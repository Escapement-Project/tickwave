// Annotations as the elements take them for now: an in-memory array with the sidecar's fields
// (PLAN.md §3). Sidecar parsing will come from the Rust core; this resolves times to ticks, gives
// generated annotations their full ids, and follows `ref`.

import type { Ratio } from "./source.ts";
import { parseTime, toTicks } from "./time.ts";

export interface Measure {
  readonly channel: string;
  readonly what: string;
}

export interface RawAnnotation {
  readonly id: string;
  readonly start?: number | string;
  readonly end?: number | string;
  readonly channels?: readonly string[];
  readonly label?: string;
  readonly kind?: string;
  readonly measure?: Measure;
  readonly paint?: string;
  readonly ref?: string;
  /** The generator that wrote it. Absent on a hand-written annotation. */
  readonly source?: string;
}

export interface Annotation {
  /** "<source>/<id>" for a generated annotation; a hand-written id has no "/". */
  readonly id: string;
  readonly start: number;
  /** Null for a point. Exclusive. */
  readonly end: number | null;
  /** Null: every channel. */
  readonly channels: readonly string[] | null;
  readonly label: string | null;
  readonly kind: string | null;
  readonly measure: Measure | null;
  readonly paint: string | null;
  readonly generated: boolean;
  /** The generated annotation this hand-written one names. */
  readonly ref: string | null;
}

export interface Resolved {
  readonly list: readonly Annotation[];
  readonly byId: ReadonlyMap<string, Annotation>;
  /** Generated annotations a hand-written one stands in for; drawn once, as the hand-written one. */
  readonly named: ReadonlySet<string>;
  readonly errors: readonly string[];
}

function time(v: number | string | undefined, timebase: Ratio): number | null | undefined {
  if (v === undefined) return undefined;
  if (typeof v === "number") return Number.isSafeInteger(v) ? v : null;
  const spec = parseTime(v);
  return spec ? toTicks(spec, timebase) : null;
}

export function resolveAnnotations(raw: readonly RawAnnotation[], timebase: Ratio): Resolved {
  const errors: string[] = [];
  const byId = new Map<string, Annotation>();
  const named = new Set<string>();
  const add = (a: Annotation) => {
    if (byId.has(a.id)) errors.push(`annotation ${a.id} repeats`);
    else byId.set(a.id, a);
  };
  const base = (r: RawAnnotation, id: string, generated: boolean): Annotation | null => {
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
      start,
      end: end ?? null,
      channels: r.channels ?? null,
      label: r.label ?? null,
      kind: r.kind ?? null,
      measure: r.measure ?? null,
      paint: r.paint ?? null,
      generated,
      ref: null,
    };
  };
  for (const r of raw) {
    if (r.source === undefined) continue;
    const a = base(r, `${r.source}/${r.id}`, true);
    if (a) add(a);
  }
  for (const r of raw) {
    if (r.source !== undefined) continue;
    if (r.id.includes("/")) {
      errors.push(`hand-written annotation ${r.id} has a "/" in its id`);
      continue;
    }
    if (r.ref === undefined) {
      const a = base(r, r.id, false);
      if (a) add(a);
      continue;
    }
    const target = byId.get(r.ref);
    if (!target?.generated) {
      errors.push(`annotation ${r.id} refers to ${r.ref}, which is not a generated annotation`);
      continue;
    }
    named.add(target.id);
    add({
      ...target,
      id: r.id,
      label: r.label ?? target.label,
      kind: r.kind ?? target.kind,
      measure: r.measure ?? target.measure,
      paint: r.paint ?? target.paint,
      generated: false,
      ref: target.id,
    });
  }
  const list = [...byId.values()].sort((a, b) => a.start - b.start || (b.end ?? b.start) - (a.end ?? a.start));
  return { list, byId, named, errors };
}

/** Whether an annotation applies to a channel. */
export function covers(a: Annotation, channel: string): boolean {
  return a.channels === null || a.channels.includes(channel);
}
