// The readout's pieces: a point's figure as text, and a series' swatch.

import type { Point, Series } from "./data.ts";
import { grouped } from "./decimal.ts";
import { marker, type Colours } from "./render.ts";

/** A point's figure, with the digits it came with: "1,238.6 KiB/s", "failed", "–". */
export function figure(p: Point | null, unit: string): string {
  if (!p) return "–";
  const u = unit ? ` ${unit}` : "";
  const wrong = p.wrong ? ", reads wrong" : "";
  if (p.value !== null) {
    const range = p.range ? ` (${grouped(p.range[0])} to ${grouped(p.range[1])})` : "";
    const note = p.note ? `, ${p.note}` : "";
    return `${grouped(p.value)}${u}${range}${note}${wrong}`;
  }
  if (p.partial) {
    const others = p.partial.of - p.partial.failed;
    return `failed in ${p.partial.failed} of ${p.partial.of} runs (${grouped(p.partial.value)}${u} in the ${others > 1 ? "others" : "other"})${wrong}`;
  }
  return p.failed ? "failed" : "–";
}

/** A series' line and marker as the chart draws them, for the legend and the readout. */
export function swatch(s: Series, c: Colours): string {
  const colour = c.series(s.colour);
  const dash = c.dash(s.pattern);
  const width = s.capture ? c.captureWidth : 2;
  const opacity = s.capture ? ` opacity="${c.captureOpacity}"` : "";
  return `<svg class="tw-swatch" width="22" height="10" aria-hidden="true"${opacity}><line x1="0" x2="22" y1="5" y2="5" stroke="${colour}" stroke-width="${width}"${dash ? ` stroke-dasharray="${dash}"` : ""}/>${marker(s.marker, 11, 5, colour, false)}</svg>`;
}
