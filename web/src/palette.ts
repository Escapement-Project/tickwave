// Colours, fonts and the line weight for the canvas, read from the host page's tokens through
// computed style. A page without the tokens gets system colours and the default weight.

import { parseDash, parseWeight, type Colour } from "./roles.ts";

/** What a page's tokens say of a paint role's look; null for each they leave to the recording. */
export interface RoleTokens {
  /** `--tickwave-role-<name>`. */
  readonly colour: string | null;
  /** `--tickwave-role-<name>-weight`, CSS pixels added to the trace's. */
  readonly weight: number | null;
  /** `--tickwave-role-<name>-dash`, (dash, gap) pairs in line widths; `none` is a solid line. */
  readonly dash: readonly number[] | "none" | null;
}

export interface Palette {
  readonly ink: string;
  readonly ink2: string;
  readonly hair: string;
  readonly rule: string;
  readonly page: string;
  /** Ink at 12 %: what the pointer is on. */
  readonly wash: string;
  /** The traces: the series colour, else the recording's stored colour, else ink. */
  readonly trace: string;
  /** Whether the page's face is its night one: its page colour darker than its ink. */
  readonly night: boolean;
  readonly fontText: string;
  readonly fontFigures: string;
  /** The traces' line, in CSS pixels: `--tickwave-line-weight`, or `LINE_WEIGHT`. */
  readonly lineWeight: number;
  /** What the page's tokens say of a paint role's look (`roleLook` takes the rest). */
  roleTokens(name: string): RoleTokens;
}

/** The traces' line where the page sets no weight, in CSS pixels. */
export const LINE_WEIGHT = 1.5;

/** A line weight as the page gives it, a number of CSS pixels with or without `px`; `LINE_WEIGHT`
 * for anything else, or a weight that is not positive and finite. */
export function lineWeight(css: string): number {
  const m = /^\s*(\+?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?)\s*(?:px)?\s*$/i.exec(css);
  const w = m ? Number(m[1]) : NaN;
  return Number.isFinite(w) && w > 0 ? w : LINE_WEIGHT;
}

let scratch: CanvasRenderingContext2D | null = null;

/** A computed colour as the canvas serialises it; null when the canvas does not parse it. */
export function normalise(css: string): string | null {
  scratch ??= document.createElement("canvas").getContext("2d");
  const ctx = scratch!;
  // Assigning an unparsable colour leaves fillStyle as it was: start from a sentinel to tell.
  ctx.fillStyle = "#010203";
  ctx.fillStyle = css;
  const out = String(ctx.fillStyle);
  return out === "#010203" && !/^(#010203|rgb\(1, 2, 3\))$/i.test(css.trim()) ? null : out;
}

let pixel: CanvasRenderingContext2D | null = null;
const painted = new Map<string, readonly [number, number, number, number]>();

/** `css` as a canvas paints it: red, green and blue from 0 to 255, and alpha from 0 to 1. */
export function rgba(css: string): readonly [number, number, number, number] {
  let c = painted.get(css);
  if (c) return c;
  pixel ??= Object.assign(document.createElement("canvas"), { width: 1, height: 1 }).getContext("2d", { willReadFrequently: true });
  const ctx = pixel!;
  ctx.clearRect(0, 0, 1, 1);
  ctx.fillStyle = css;
  ctx.fillRect(0, 0, 1, 1);
  const d = ctx.getImageData(0, 0, 1, 1).data;
  c = [d[0], d[1], d[2], d[3] / 255];
  painted.set(css, c);
  return c;
}

const FALLBACK: Record<string, string> = {
  "--ink": "CanvasText",
  "--ink-2": "GrayText",
  "--hair": "color-mix(in srgb, CanvasText 22%, Canvas)",
  "--rule": "GrayText",
  "--page": "Canvas",
};

/** Whether `page` is darker than `ink`, by their relative luminance. */
function darker(page: string, ink: string): boolean {
  const lum = (css: string) => {
    const [r, g, b] = rgba(css).map((c, i) => (i < 3 ? (c / 255) ** 2.2 : c));
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  return lum(page) < lum(ink);
}

/**
 * The page's palette, read through `probe`. `stored` is the recording's own trace colour
 * (ANNOTATIONS.md, "The stored view"), under the page's: the trace takes `--series-<series>`
 * where the page names a series and sets it, else the stored colour on this face, else ink.
 */
export function resolvePalette(probe: HTMLElement, series: string | null, stored: Colour | null = null): Palette {
  const read = (expr: string, fallback: string) => {
    probe.style.color = expr;
    return normalise(getComputedStyle(probe).color) ?? fallback;
  };
  const token = (name: string) => read(`var(${name}, ${FALLBACK[name] ?? "CanvasText"})`, "rgb(0, 0, 0)");
  const ink = token("--ink");
  const page = token("--page");
  const night = darker(page, ink);
  const inkExpr = `var(--ink, CanvasText)`;
  const own = stored ? (night ? stored.night : stored.day) : inkExpr;
  const seriesExpr = series ? `var(--series-${series}, ${own})` : own;
  const roles = new Map<string, RoleTokens>();
  const roleTokens = (name: string): RoleTokens => {
    let t = roles.get(name);
    if (t === undefined) {
      const style = getComputedStyle(probe);
      const colour = style.getPropertyValue(`--tickwave-role-${name}`).trim();
      const dash = style.getPropertyValue(`--tickwave-role-${name}-dash`).trim();
      t = {
        colour: colour === "" ? null : read(`var(--tickwave-role-${name})`, ink),
        weight: parseWeight(style.getPropertyValue(`--tickwave-role-${name}-weight`)),
        dash: dash === "none" ? "none" : parseDash(dash),
      };
      roles.set(name, t);
    }
    return t;
  };
  const font = (expr: string) => {
    probe.style.fontFamily = expr;
    return getComputedStyle(probe).fontFamily;
  };
  const palette: Palette = {
    ink,
    ink2: token("--ink-2"),
    hair: token("--hair"),
    rule: token("--rule"),
    page,
    night,
    wash: read(`var(--wash, color-mix(in srgb, ${inkExpr} 12%, transparent))`, "rgba(0, 0, 0, 0.12)"),
    trace: read(seriesExpr, ink),
    fontText: font("var(--font-text, system-ui, sans-serif)"),
    fontFigures: font("var(--font-figures, ui-monospace, monospace)"),
    lineWeight: lineWeight(getComputedStyle(probe).getPropertyValue("--tickwave-line-weight")),
    roleTokens,
  };
  return palette;
}
