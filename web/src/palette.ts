// Colours and fonts for the canvas, read from the host page's tokens through computed style.
// A page without the tokens gets system colours.

export interface Palette {
  readonly ink: string;
  readonly ink2: string;
  readonly hair: string;
  readonly rule: string;
  readonly page: string;
  /** Ink at 12 %: what the pointer is on. */
  readonly wash: string;
  /** The traces: the series colour, or ink. */
  readonly trace: string;
  readonly fontText: string;
  readonly fontFigures: string;
  /** A paint role's colour. */
  role(name: string): string;
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

export function resolvePalette(probe: HTMLElement, series: string | null): Palette {
  const read = (expr: string, fallback: string) => {
    probe.style.color = expr;
    return normalise(getComputedStyle(probe).color) ?? fallback;
  };
  const token = (name: string) => read(`var(${name}, ${FALLBACK[name] ?? "CanvasText"})`, "rgb(0, 0, 0)");
  const ink = token("--ink");
  const inkExpr = `var(--ink, CanvasText)`;
  const seriesExpr = series ? `var(--series-${series}, ${inkExpr})` : inkExpr;
  const roles = new Map<string, string>();
  const role = (name: string) => {
    let c = roles.get(name);
    if (c === undefined) {
      const fallback = name === "probe" ? seriesExpr : inkExpr;
      c = read(`var(--tickwave-${name}, ${fallback})`, ink);
      roles.set(name, c);
    }
    return c;
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
    page: token("--page"),
    wash: read(`var(--wash, color-mix(in srgb, ${inkExpr} 12%, transparent))`, "rgba(0, 0, 0, 0.12)"),
    trace: read(seriesExpr, ink),
    fontText: font("var(--font-text, system-ui, sans-serif)"),
    fontFigures: font("var(--font-figures, ui-monospace, monospace)"),
    role,
  };
  // Resolve the two named roles now, while the probe's styles are this face's.
  role("probe");
  role("target");
  return palette;
}
