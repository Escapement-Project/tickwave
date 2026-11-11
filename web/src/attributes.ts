// Every attribute the elements read, parsed in one place: a change to the element API lands here
// and in the observed lists only.

import { parseTime, type Seconds, type TimeSpec } from "./time.ts";
import { SERIES_TOKEN } from "./tokens.ts";

export const VIEW_ATTRIBUTES = ["src", "annotations", "channels", "window", "open", "series"];
export const INSET_ATTRIBUTES = ["src", "annotations", "annotation", "channels", "pad", "scale", "sweep", "series", "for", "href"];
export const REF_ATTRIBUTES = ["annotation", "for", "src", "annotations", "series"];

/** Channel names in display order; null for every channel. */
export function parseChannels(v: string | null): string[] | null {
  const names = v?.trim().split(/\s+/).filter(Boolean) ?? [];
  return names.length > 0 ? names : null;
}

/** A series name: the trace and the probe role take `--series-<name>`. Null for ink. */
export function parseSeries(v: string | null): string | null {
  const s = v?.trim() ?? "";
  return SERIES_TOKEN.test(s) ? s : null;
}

export type WindowSpec =
  | { readonly kind: "span"; readonly start: TimeSpec; readonly end: TimeSpec }
  | { readonly kind: "annotation"; readonly id: string };

const TIME = String.raw`-?\d+(?:\.\d+)?(?:\s*[a-zµμ]+)?`;

/** "317.8 ms 318.0 ms", "1000 2000" (ticks), or an annotation id. */
export function parseWindow(v: string | null): WindowSpec | null {
  const t = v?.trim() ?? "";
  if (t === "") return null;
  const m = new RegExp(`^(${TIME})\\s+(${TIME})$`, "i").exec(t);
  if (m) {
    const start = parseTime(m[1]);
    const end = parseTime(m[2]);
    if (start && end) return { kind: "span", start, end };
  }
  return /\s/.test(t) ? null : { kind: "annotation", id: t };
}

export type PadSpec = { readonly fraction: number } | { readonly time: TimeSpec };

export const DEFAULT_PAD: PadSpec = { fraction: 0.06 };

/** "10%" of the span on each side, or a time ("50 ns", "200" ticks). */
export function parsePad(v: string | null): PadSpec {
  const t = v?.trim() ?? "";
  const pct = /^(\d+(?:\.\d+)?)\s*%$/.exec(t);
  if (pct) return { fraction: Number(pct[1]) / 100 };
  const time = parseTime(t);
  return time ? { time } : DEFAULT_PAD;
}

/** "2 ns/px": seconds a CSS pixel. Null when absent or unreadable. */
export function parseScale(v: string | null): Seconds | null {
  const m = /^(.*\S)\s*\/\s*px$/i.exec(v?.trim() ?? "");
  const time = m ? parseTime(m[1]) : null;
  return time && "seconds" in time && time.seconds.mantissa > 0n ? time.seconds : null;
}

/** `sweep="off"` draws at once. */
export function parseSweep(v: string | null): boolean {
  return v?.trim().toLowerCase() !== "off";
}
