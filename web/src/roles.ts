// Paint roles (ANNOTATIONS.md, "Roles"): what a sidecar declares, each role's look, field by field,
// from the page's tokens, then the declaration, and for its colour the role's place in the order of
// declarations, and which role draws each stretch of a row. The view core's `roles` module ports
// it; both are held to testdata/view/roles.json.

import type { Palette } from "./palette.ts";

/** A role's colour on the page's day face and on its night face, each `#rrggbb`. */
export interface Colour {
  readonly day: string;
  readonly night: string;
}

/** A role as a sidecar declares it, with its defaults: a label left out is the name, a weight 0. */
export interface Role {
  readonly name: string;
  readonly label: string;
  readonly colour: Colour | null;
  /** Added to the trace's line weight, in CSS pixels. */
  readonly weight: number;
  /** (dash, gap) pairs in the role's line widths; null for a solid line. */
  readonly dash: readonly number[] | null;
}

/** How a role's lines draw, and what its key says. */
export interface Look {
  readonly label: string;
  readonly colour: string;
  /** Added to the trace's line weight, in CSS pixels. */
  readonly weight: number;
  /** (dash, gap) pairs in the role's line widths; null for a solid line. */
  readonly dash: readonly number[] | null;
}

/** The most a weight adds, in CSS pixels, and the longest dash or gap, in line widths. */
export const WEIGHT_AT_MOST = 4;
export const DASH_AT_MOST = 64;

/** A key's swatch for `look`: a short line as the role's lines draw, `lineWeight` the trace's in
 * CSS pixels, its dash in line widths. */
export function swatch(look: Look, lineWeight: number): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const width = lineWeight + look.weight;
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("width", "18");
  svg.setAttribute("height", "8");
  svg.setAttribute("aria-hidden", "true");
  const line = document.createElementNS(ns, "line");
  for (const [k, v] of [
    ["x1", "0"],
    ["x2", "18"],
    ["y1", "4"],
    ["y2", "4"],
    ["stroke", look.colour],
    ["stroke-width", String(width)],
  ])
    line.setAttribute(k, v);
  if (look.dash) line.setAttribute("stroke-dasharray", look.dash.map((d) => d * width).join(" "));
  svg.append(line);
  return svg;
}

/** A dash a page gives, `--tickwave-role-<name>-dash`: numbers apart by spaces or commas, (dash,
 * gap) pairs, at most 8, each from 0 to 64 line widths and not all 0; null for anything else. */
export function parseDash(css: string): number[] | null {
  const text = css.trim();
  if (text === "") return null;
  const numbers = text.split(/[\s,]+/).map(Number);
  const fits = numbers.length % 2 === 0 && numbers.length <= 8 && numbers.every((n) => Number.isFinite(n) && n >= 0 && n <= DASH_AT_MOST);
  return fits && numbers.reduce((a, b) => a + b, 0) > 0 ? numbers : null;
}

/** A weight a page gives, `--tickwave-role-<name>-weight`: CSS pixels, with or without `px`, from
 * 0 to 4; null for anything else. */
export function parseWeight(css: string): number | null {
  const m = /^\s*(\+?(?:\d+\.?\d*|\.\d+))\s*(?:px)?\s*$/i.exec(css);
  const w = m ? Number(m[1]) : NaN;
  return Number.isFinite(w) && w >= 0 && w <= WEIGHT_AT_MOST ? w : null;
}

/**
 * Role `name`'s look, each field from the first that gives it: the page (`--tickwave-role-<name>`,
 * `-weight`, `-dash`, read through `p.roleTokens`), then the recording's declaration. A colour
 * neither gives is the place's in `roles`, the order the sidecar declares them in: the first role
 * takes the trace's colour, the rest ink. A dash neither gives is solid, and a weight the
 * declaration's, 0 where it gives none. Null for a role the sidecar doesn't declare: a paint naming
 * one draws as if it painted nothing.
 */
export function roleLook(p: Palette, roles: readonly Role[], name: string): Look | null {
  const at = roles.findIndex((r) => r.name === name);
  if (at < 0) return null;
  const declared = roles[at];
  const page = p.roleTokens(name);
  const stored = declared.colour ? (p.night ? declared.colour.night : declared.colour.day) : null;
  const dash = page.dash ?? declared.dash;
  return {
    label: declared.label,
    colour: page.colour ?? stored ?? (at === 0 ? p.trace : p.ink),
    weight: page.weight ?? declared.weight,
    dash: dash === "none" ? null : dash,
  };
}

/** A paint over a row, `from` to `to` in its columns, in role `role`. */
export interface PaintSpan {
  readonly from: number;
  readonly to: number;
  readonly role: string;
}

/** A stretch of a row's columns, and the role that draws it: null for the trace's own look. */
export interface PaintStretch {
  readonly px0: number;
  readonly px1: number;
  readonly role: string | null;
}

/**
 * Columns `px0` to `px1` of a row cut where the role drawing them changes. `spans` are the paints
 * over the row in sidecar order; where several cover a column, the latest draws it. A span naming a
 * role `roles` doesn't declare draws nothing, so what lies under it shows. Neighbours in one role
 * are one stretch.
 */
export function paintStretches(spans: readonly PaintSpan[], roles: readonly Role[], px0: number, px1: number): PaintStretch[] {
  const names = new Set(roles.map((r) => r.name));
  const marks: { at: number; i: number; on: boolean }[] = [];
  const kept: string[] = [];
  for (const s of spans) {
    const from = Math.max(px0, s.from);
    const to = Math.min(px1, s.to);
    if (to <= from || !names.has(s.role)) continue;
    marks.push({ at: from, i: kept.length, on: true }, { at: to, i: kept.length, on: false });
    kept.push(s.role);
  }
  marks.sort((m, n) => m.at - n.at);
  const out: PaintStretch[] = [];
  const active = new Set<number>();
  const stretch = (from: number, to: number) => {
    const role = active.size === 0 ? null : kept[Math.max(...active)];
    const last = out[out.length - 1];
    if (last && last.role === role && last.px1 === from) out[out.length - 1] = { px0: last.px0, px1: to, role };
    else out.push({ px0: from, px1: to, role });
  };
  let x = px0;
  for (const m of marks) {
    if (m.at > x) {
      stretch(x, m.at);
      x = m.at;
    }
    if (m.on) active.add(m.i);
    else active.delete(m.i);
  }
  if (x < px1) stretch(x, px1);
  return out;
}

/**
 * The length of a row's line at device-pixel column `x`, where a dashed stretch starting there
 * starts its pattern: the last point at or before `x`, and the way along the segment after it.
 * `paths` are the line's paths in order, each its points' x and the line's length there, flat. A
 * lifted pen adds nothing, so between two paths the length is the first's at its end.
 */
export function lengthAt(paths: readonly { readonly points?: readonly number[] }[], x: number): number {
  let lo = 0;
  let hi = paths.length;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (paths[mid].points![0] <= x) lo = mid;
    else hi = mid;
  }
  const pts = paths[lo]?.points;
  if (!pts) return 0;
  let k = 0;
  while (k + 2 < pts.length && pts[k + 2] <= x) k += 2;
  if (k + 2 >= pts.length || pts[k] > x) return pts[k + 1];
  return pts[k + 1] + ((x - pts[k]) * (pts[k + 3] - pts[k + 1])) / (pts[k + 2] - pts[k]);
}
