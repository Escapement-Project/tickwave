// The readout's pieces: a point's figure as text, and a series' swatch.

import { escapeHtml } from "../escape.ts";
import type { Point, Series } from "./data.ts";
import { grouped } from "./decimal.ts";
import { marker, paint, type Colours } from "./render.ts";

/** A point's figure, with the digits it came with, and its note: "1,238.6 KiB/s", "failed", "–". */
export function figure(p: Point | null, unit: string): string {
  if (!p) return "–";
  const u = unit ? ` ${unit}` : "";
  const wrong = p.wrong ? ", reads wrong" : "";
  const note = p.note ? `, ${p.note}` : "";
  if (p.value !== null) {
    const range = p.range ? ` (${grouped(p.range[0])} to ${grouped(p.range[1])})` : "";
    return `${grouped(p.value)}${u}${range}${note}${wrong}`;
  }
  if (p.partial) {
    const others = p.partial.of - p.partial.failed;
    return `failed in ${p.partial.failed} of ${p.partial.of} runs (${grouped(p.partial.value)}${u} in the ${others > 1 ? "others" : "other"})${note}${wrong}`;
  }
  if (p.failed) return `failed${note}`;
  return p.note ?? "–";
}

/** A series' line and marker as the chart draws them, for the legend and the readout. Inline in its
 * text whatever the page's own rules for `svg` say: a reset's `display: block` would put each on
 * a line of its own. */
export function swatch(s: Series, c: Colours): string {
  const colour = c.series(s.colour);
  const dash = c.dash(s.pattern);
  const width = s.capture ? c.captureWidth : 2;
  const opacity = s.capture ? ` opacity="${c.captureOpacity}"` : "";
  return `<svg class="tw-swatch" style="display:inline-block" width="22" height="10" aria-hidden="true"${opacity}><line x1="0" x2="22" y1="5" y2="5" ${paint("stroke", colour)} stroke-width="${width}"${dash ? ` stroke-dasharray="${escapeHtml(dash)}"` : ""}/>${marker(s.marker, 11, 5, colour, false)}</svg>`;
}
