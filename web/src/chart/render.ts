// A chart's SVG, as markup. Data colours are presentation attributes, resolved from the page's tokens
// when it draws; text, grid and neutral marks are currentColor; nothing refers to an id. So the
// figure reads the same in reader view, in a feed, or without the page's CSS. Where the page's CSS
// applies, each data colour's style refers to its token instead, so the colours follow the page's
// face without a redraw. The rings that part a point from
// what it overlaps, and a hollow point's middle, take the surface colour from CSS only (`tw-pt`,
// `tw-hollow`): without it they drop rather than keep a stale colour.

import { escapeHtml as esc } from "../escape.ts";
import type { Marker, Pattern } from "./data.ts";
import { f1, Y_LABEL_GAP, type Layout, type Placed, type SeriesLayout } from "./layout.ts";

/** A data colour: its value when the chart drew, and the CSS that gives it, following the face. */
export interface Paint {
  readonly value: string;
  readonly css: string;
}

export interface Colours {
  /** A series token's colour: `--series-<name>`. */
  series(name: string): Paint;
  /** A line pattern's dash array; empty for solid. */
  dash(pattern: Pattern | "misread"): string;
  readonly fault: Paint;
  readonly captureWidth: number;
  readonly captureOpacity: number;
  readonly fontText: string;
}

export interface State {
  /** The setting picked, by index. */
  readonly x: number | null;
  readonly series: string | null;
  readonly limit: boolean;
  /** Points to ring: [series id, setting index]. */
  readonly rings: readonly (readonly [string, number])[];
  /** The sweep's x, while it plays; null once the lines are whole. */
  readonly sweep: number | null;
  /** A share of each end label's characters, while they show; null once whole. */
  readonly labels: number | null;
  readonly endLabels: boolean;
  readonly xName: string;
  readonly yUnit: string;
}


/** `prop` in `p`: the attribute for a reader without the page's CSS, the style for one with it. */
export function paint(prop: "fill" | "stroke", p: Paint): string {
  return `${prop}="${esc(p.value)}" style="${prop}:${esc(p.css)}"`;
}

export function render(l: Layout, state: State, c: Colours): string {
  const out: string[] = [];
  const text = (x: number, y: number, anchor: string, body: string, extra = "") =>
    `<text x="${f1(x)}" y="${f1(y)}" text-anchor="${anchor}" fill="currentColor" fill-opacity=".72"${extra}>${esc(body)}</text>`;

  for (const t of l.yTicks) {
    out.push(`<line x1="${l.left}" x2="${f1(l.right)}" y1="${f1(t.y)}" y2="${f1(t.y)}" stroke="currentColor" stroke-opacity=".2"/>`);
    out.push(text(l.left - Y_LABEL_GAP, t.y + 4, "end", t.text));
  }
  for (const t of l.xLabels) out.push(text(t.x, l.bottom + 18, "middle", t.text));
  if (state.xName) out.push(text(l.right, l.height - 6, "end", state.xName));
  if (state.yUnit) out.push(text(l.left - Y_LABEL_GAP, l.top - 14, "end", state.yUnit));
  if (l.reference) out.push(`<path d="${l.reference}" fill="none" stroke="currentColor" stroke-opacity=".5" stroke-dasharray="4 4"/>`);

  l.limits.forEach((m, i) => {
    let y1 = l.top;
    let y2 = l.bottom;
    if (state.sweep !== null) {
      // Before the sweep reaches it, nothing; then it grows from its series' point, if it has one.
      if (state.sweep < m.x) return;
      if (m.origin !== null && m.reach !== null) {
        const f = Math.min(1, (state.sweep - m.x) / (m.reach - m.x));
        y1 = m.origin - f * (m.origin - l.top);
        y2 = m.origin + f * (l.bottom - m.origin);
      }
    }
    const width = state.limit ? 2 : 1;
    out.push(`<line class="tw-limit" data-limit="${i}" x1="${f1(m.x)}" x2="${f1(m.x)}" y1="${f1(y1)}" y2="${f1(y2)}" stroke="currentColor" stroke-opacity=".72" stroke-width="${width}" stroke-dasharray="3 4"/>`);
  });

  for (const s of l.series) out.push(seriesMarkup(s, l, state, c));

  for (const f of l.fails) {
    const dim = state.series !== null && state.series !== f.series ? ' opacity=".2"' : "";
    out.push(`<g${dim}><path d="M${f1(f.x - 4)} ${f1(f.y - 4)}l8 8m0 -8l-8 8" ${paint("stroke", c.fault)} stroke-width="2" fill="none"/>`);
    out.push(`<text x="${f1(f.x - 9)}" y="${f1(f.y + 4)}" text-anchor="end" ${paint("fill", c.fault)} font-weight="600">${esc(f.text)}</text></g>`);
  }

  for (const [id, i] of state.rings) {
    const p = l.series.find((s) => s.series.id === id)?.placed.find((q) => q.index === i);
    if (p) out.push(`<circle cx="${f1(p.x)}" cy="${f1(p.y)}" r="9" fill="none" stroke="currentColor" stroke-width="2"/>`);
  }
  if (state.x !== null) {
    const x = l.xs[state.x];
    out.push(`<line x1="${f1(x)}" x2="${f1(x)}" y1="${l.top}" y2="${f1(l.bottom)}" stroke="currentColor" stroke-opacity=".35"/>`);
  }
  out.push(`<rect class="tw-hit" x="${l.left}" y="${l.top}" width="${f1(l.right - l.left)}" height="${f1(l.bottom - l.top)}" fill="transparent"/>`);
  return out.join("");
}

function seriesMarkup(s: SeriesLayout, l: Layout, state: State, c: Colours): string {
  const { series } = s;
  const colour = c.series(series.colour);
  const width = series.capture ? c.captureWidth : 2;
  const dim = state.series !== null && state.series !== series.id;
  const opacity = (series.capture ? c.captureOpacity : 1) * (dim ? 0.2 : 1);
  const sweep = state.sweep;
  const out: string[] = [`<g class="tw-series" data-series="${esc(series.id)}"${opacity < 1 ? ` opacity="${opacity}"` : ""}>`];

  // A run of joined segments that share a dash is one path; the segment into a point read wrong
  // is dotted. A failed setting leaves the next segment unjoined: it starts a path of its own.
  const runs: { wrong: boolean; d: string; to: Placed }[] = [];
  for (const seg of s.segments) {
    if (sweep !== null && sweep <= seg.from.x) break;
    const to = sweep !== null && sweep < seg.to.x ? cut(seg.from, seg.to, sweep) : seg.to;
    const run = runs[runs.length - 1];
    if (run && run.wrong === seg.wrong && run.to === seg.from) {
      run.d += `L${f1(to.x)} ${f1(to.y)}`;
      run.to = seg.to;
    } else runs.push({ wrong: seg.wrong, d: `M${f1(seg.from.x)} ${f1(seg.from.y)}L${f1(to.x)} ${f1(to.y)}`, to: seg.to });
  }
  for (const run of runs) {
    const dash = run.wrong ? c.dash("misread") : c.dash(series.pattern);
    out.push(`<path d="${run.d}" fill="none" ${paint("stroke", colour)} stroke-width="${width}"${dash ? ` stroke-dasharray="${esc(dash)}"` : ""}/>`);
  }
  for (const p of s.placed) {
    if (sweep !== null && sweep < p.x) break;
    out.push(marker(series.marker, p.x, p.y, colour, p.point.wrong));
  }
  if (state.endLabels && s.label && (sweep === null || sweep >= l.sweepTo)) {
    const chars = [...series.short];
    const shown = state.labels === null ? chars.length : Math.ceil(state.labels * chars.length);
    if (shown > 0) {
      out.push(`<text class="tw-end" x="${f1(s.label.x)}" y="${f1(s.label.y + 4)}" fill="currentColor" font-weight="600">${esc(chars.slice(0, shown).join(""))}</text>`);
    }
  }
  out.push("</g>");
  return out.join("");
}

function cut(a: Placed, b: Placed, x: number): { x: number; y: number } {
  const f = (x - a.x) / (b.x - a.x);
  return { x, y: a.y + f * (b.y - a.y) };
}

/** A point's marker, 8 px across: filled with a ring in the surface colour, or hollow. */
export function marker(shape: Marker, x: number, y: number, colour: Paint, hollow: boolean): string {
  const look = hollow
    ? `class="tw-hollow" fill="none" ${paint("stroke", colour)} stroke-width="2"`
    : `class="tw-pt" ${paint("fill", colour)} stroke="none" stroke-width="2"`;
  const X = f1(x);
  const Y = f1(y);
  switch (shape) {
    case "circle":
      return `<circle cx="${X}" cy="${Y}" r="4" ${look}/>`;
    case "square":
      return `<rect x="${f1(x - 3.5)}" y="${f1(y - 3.5)}" width="7" height="7" ${look}/>`;
    case "diamond":
      return `<path d="M${X} ${f1(y - 5)}l5 5l-5 5l-5 -5z" ${look}/>`;
    case "triangle-up":
      return `<path d="M${X} ${f1(y - 5)}l5 8.5h-10z" ${look}/>`;
    case "triangle-down":
      return `<path d="M${X} ${f1(y + 5)}l5 -8.5h-10z" ${look}/>`;
  }
}
