// <tickwave-chart>: measurements against a measured setting (CHARTS.md). It draws into its own
// children, not a shadow root, so reader view and feeds keep the figure.

import { autoUpdate, computePosition, flip, offset, shift } from "@floating-ui/dom";
import { escapeHtml } from "../escape.ts";
import { onFaceChange, onFontsLoaded, reducedMotion } from "../face.ts";
import { normalise } from "../palette.ts";
import { parseChart, type ChartData, type Evidence, type Pattern } from "./data.ts";
import { grouped } from "./decimal.ts";
import { layout, type Layout, type Placed, type SeriesLayout } from "./layout.ts";
import { figure, swatch } from "./readout.ts";
import { render, type Colours } from "./render.ts";

const SVG = "http://www.w3.org/2000/svg";
const SWEEP_MS = 750;
const LABELS_MS = 200;
const DEFAULT_HEIGHT = 330;
/** How near the pointer must be to a point, in CSS pixels, to point at it. */
const REACH = 12;
/** How long a mouse must rest on a point to open it: one passing over the plot loads nothing. */
const DWELL_MS = 150;
const EVIDENCE_HINT = "Enter opens the recording behind the picked point.";

// Low specificity (`:where`), so the page's own rules win.
const CSS = `
:where(tickwave-chart) { display: block; position: relative; }
:where(tickwave-chart):focus-visible { outline: var(--focus-width, 2px) solid var(--ink, CanvasText); outline-offset: 4px; }
:where(tickwave-chart[data-pending]) .tw-data { visibility: hidden; }
:where(tickwave-chart) .tw-pt { stroke: var(--surface, Canvas); }
:where(tickwave-chart) .tw-hollow { fill: var(--surface, Canvas); }
:where(tickwave-chart) .tw-plot { display: block; overflow: visible; }
:where(tickwave-chart) .tw-legend { list-style: none; display: flex; flex-wrap: wrap; gap: 4px 18px; margin: 0 0 6px;
  padding: 0; font-size: 13px; }
:where(tickwave-chart) .tw-legend li { display: flex; align-items: center; gap: 6px; }
:where(tickwave-chart) .tw-swatch { flex: none; vertical-align: -1px; margin-right: 6px; }
:where(tickwave-chart) .tw-readout { font-size: 13px; line-height: 1.45; }
:where(tickwave-chart) .tw-readout[data-tooltip] { position: absolute; z-index: 1; pointer-events: none; white-space: nowrap;
  background: var(--raised, Canvas); border: 1px solid var(--hair, GrayText); border-radius: 6px; padding: 6px 9px; }
:where(tickwave-chart) .tw-readout[data-tooltip]:empty { display: none; }
:where(tickwave-chart) .tw-readout b { font-weight: 600; }
:where(tickwave-chart) .tw-notice { margin: 4px 0 0; padding: 1px 8px; font-size: 12px; color: var(--attention-ink, CanvasText);
  border-left: 2px solid var(--attention-ink, CanvasText); }
:where(tickwave-chart) .tw-pop { position: fixed; inset: auto; margin: 0; box-sizing: border-box; width: min(440px, calc(100vw - 16px));
  padding: 8px 6px 4px; color: var(--ink, CanvasText); background: var(--page, Canvas);
  border: 1px solid var(--hair, GrayText); border-radius: 10px; box-shadow: 0 2px 10px rgba(0, 0, 0, .14); overflow: visible;
  pointer-events: none; }
:where(tickwave-chart) .tw-pop-head { display: flex; align-items: center; margin: 0 4px 4px; font-size: 13px; }
`;

/** The other elements, from the tickwave.js beside this module, the first time a point's evidence
 * opens. A page that already loaded them, from wherever, keeps its own. */
let elements: Promise<unknown> | null = null;
function loadElements(): Promise<unknown> {
  if (customElements.get("tickwave-inset")) return Promise.resolve();
  return (elements ??= import(new URL("tickwave.js", import.meta.url).href));
}

/** A shown point, or a failure's mark, that names a recording. */
interface Pointed {
  readonly series: SeriesLayout;
  readonly at: Placed;
  readonly evidence: Evidence;
}

let styled = false;
function addStyle(): void {
  if (styled) return;
  styled = true;
  const style = document.createElement("style");
  style.dataset.tickwaveChart = "";
  style.textContent = CSS;
  document.head.prepend(style);
}

/** Charts that share a selected setting, by `group`. */
const groups = new Map<string, Set<TickwaveChart>>();

export class TickwaveChart extends HTMLElement {
  static observedAttributes = ["src", "show", "selected", "group", "highlight", "readout", "legend", "end-labels", "table", "height", "sweep"];

  #data: ChartData | null = null;
  #layout: Layout | null = null;
  #svg: SVGSVGElement | null = null;
  #legend: HTMLUListElement | null = null;
  #readout: HTMLDivElement | null = null;
  #notice: HTMLParagraphElement | null = null;
  #probe: HTMLSpanElement | null = null;
  #colours: Colours | null = null;
  #x: number | null = null;
  #series: string | null = null;
  #limit = false;
  /** 0 to 1 while the lines sweep in, then the end labels; null at rest. */
  #sweep: number | null = null;
  #labels: number | null = null;
  #swept = false;
  #loadedSrc: string | null = null;
  #cleanups: (() => void)[] = [];
  #settle: () => void = () => {};
  readonly #settled = new Promise<void>((resolve) => (this.#settle = resolve));
  #ready = false;
  /** At least 60 % of the chart is in view: the sweep starts once it also has data. */
  #visible = false;
  /** The series picked by pointing at its end label, which leaving the label unpicks. */
  #fromLabel = false;
  /** The markup last written to the legend and the readout. Their `innerHTML` reads back
   * serialised differently, so comparing with it would rewrite them, and lose the pointer's and the
   * focus's place, on every draw. */
  #legendHtml = "";
  #readoutHtml = "";
  #popHeadHtml = "";
  #connected = false;
  /** The URL evidence is relative to: the data's file, or the page for data set directly. */
  #base = document.baseURI;
  #pop: HTMLDivElement | null = null;
  /** The popover's first line: the series whose recording it shows. */
  #popHead: HTMLParagraphElement | null = null;
  #inset: HTMLElement | null = null;
  /** The point whose evidence the popover shows, or is loading to show. */
  #pointed: Pointed | null = null;
  #stopPosition: (() => void) | null = null;
  /** The point the mouse rests on, and the timer that opens it. */
  #resting: Pointed | null = null;
  #dwell = 0;
  /** Says, once, that Enter opens a point's recording; in the description while any point names one. */
  #hint: HTMLSpanElement | null = null;

  constructor() {
    super();
    this.addEventListener("keydown", (e) => this.#key(e));
    this.addEventListener("focusout", (e) => {
      if (!this.contains(e.relatedTarget as Node | null)) this.#closeEvidence();
    });
  }

  connectedCallback(): void {
    this.#connected = true;
    addStyle();
    if (!this.hasAttribute("tabindex")) this.tabIndex = 0;
    this.setAttribute("role", "group");
    this.setAttribute("aria-roledescription", "chart");
    this.#name();
    this.#build();
    // `data` set on the element before this class defined it is an own property that hides the
    // setter: take it through the setter.
    if (Object.hasOwn(this, "data")) {
      const value = (this as { data?: unknown }).data;
      delete (this as { data?: unknown }).data;
      this.data = value;
    }
    const resize = new ResizeObserver(() => this.#draw());
    resize.observe(this);
    // Shown once 60 % of it is, or, for a chart taller than the window, once it fills 60 % of the
    // window. isIntersecting alone is true at any share, the first report included.
    const seen = new IntersectionObserver(
      (entries) => {
        this.#visible = entries.some(
          (e) => e.isIntersecting && (e.intersectionRatio >= 0.6 || e.intersectionRect.height >= 0.6 * (e.rootBounds?.height ?? Infinity)),
        );
        if (this.#visible) this.#startSweep();
      },
      { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6] },
    );
    seen.observe(this);
    // A press in any chart of the group is a pick there, which moves this one. The path, not the
    // target: a pick redraws the plot and detaches the target before this runs.
    const outside = (e: PointerEvent) => {
      const path = e.composedPath();
      const group = groups.get(this.getAttribute("group") ?? "") ?? [this];
      if (![...group].some((chart) => path.includes(chart))) this.#clear();
    };
    const print = () => this.#finishSweep();
    const escape = (e: KeyboardEvent) => {
      if (e.key === "Escape") this.#closeEvidence();
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    addEventListener("beforeprint", print);
    this.#cleanups.push(
      () => resize.disconnect(),
      () => seen.disconnect(),
      () => document.removeEventListener("pointerdown", outside),
      () => document.removeEventListener("keydown", escape),
      () => removeEventListener("beforeprint", print),
      () => this.#closeEvidence(),
      onFaceChange(() => {
        this.#colours = null;
        this.#draw();
      }),
      onFontsLoaded(() => this.#draw()),
    );
    this.#join(this.getAttribute("group"));
    void this.#load();
  }

  disconnectedCallback(): void {
    this.#connected = false;
    for (const f of this.#cleanups.splice(0)) f();
    this.#join(null);
  }

  attributeChangedCallback(name: string, old: string | null, value: string | null): void {
    // Before connectedCallback, an upgrade's attributes included: it reads them all.
    if (!this.#connected || old === value) return;
    if (name === "src") void this.#load();
    else if (name === "group") this.#join(value);
    else if (name === "selected") {
      this.#x = this.#indexOf(value);
      this.#draw();
    } else if (name === "table" || name === "legend") {
      this.#name();
      this.#build();
      this.#draw();
    } else this.#draw();
  }

  /** The chart's data. Setting it redraws without replaying the sweep. */
  get data(): ChartData | null {
    return this.#data;
  }

  set data(value: unknown) {
    this.#loadedSrc = null;
    this.#base = document.baseURI;
    this.#take(() => parseChart(value));
  }

  /** Selects a setting, a series or both, as a reader would, and moves the group with it;
   * `pick()` clears. Fires no event. */
  pick(x: string | null = null, series: string | null = null): void {
    const before = this.#x;
    this.#x = x === null ? (this.#sticky ? this.#x : null) : this.#indexOf(x);
    this.#series = series;
    this.#limit = false;
    this.#draw();
    if (this.#x !== before) this.#moveGroup();
  }

  /** Resolves once the chart has drawn, its sweep included. */
  settled(): Promise<void> {
    return this.#settled;
  }

  get #sticky(): boolean {
    return this.hasAttribute("selected");
  }

  #indexOf(x: string | null): number | null {
    const i = x === null || !this.#data ? -1 : this.#data.x.values.indexOf(x);
    return i < 0 ? null : i;
  }

  #name(): void {
    const caption = this.closest("figure")?.querySelector(":scope > figcaption");
    if (caption && !this.hasAttribute("aria-label")) {
      caption.id ||= `tw-caption-${Math.random().toString(36).slice(2, 9)}`;
      this.setAttribute("aria-labelledby", caption.id);
    }
    const evidence = this.#targets().length > 0;
    const ids = [this.getAttribute("table"), evidence ? this.#hint?.id : null].filter(Boolean).join(" ");
    if (!ids) this.removeAttribute("aria-describedby");
    else if (this.getAttribute("aria-describedby") !== ids) this.setAttribute("aria-describedby", ids);
  }

  #build(): void {
    this.#legend = this.getAttribute("legend") === "above" ? document.createElement("ul") : null;
    this.#legend?.classList.add("tw-legend");
    this.#svg = document.createElementNS(SVG, "svg");
    this.#svg.classList.add("tw-plot");
    this.#svg.setAttribute("font-size", "12");
    this.#legendHtml = this.#readoutHtml = "";
    this.#readout = document.createElement("div");
    this.#readout.className = "tw-readout";
    this.#readout.setAttribute("aria-live", "polite");
    this.#notice = document.createElement("p");
    this.#notice.className = "tw-notice";
    this.#notice.hidden = true;
    this.#probe = document.createElement("span");
    this.#probe.hidden = true;
    this.#hint = document.createElement("span");
    this.#hint.hidden = true;
    this.#hint.id = `tw-hint-${Math.random().toString(36).slice(2, 9)}`;
    this.#hint.textContent = EVIDENCE_HINT;
    this.#closeEvidence();
    this.#pop = document.createElement("div");
    this.#pop.className = "tw-pop";
    this.#pop.setAttribute("popover", "manual");
    this.#popHead = this.#pop.appendChild(document.createElement("p"));
    this.#popHead.className = "tw-pop-head";
    this.#inset = null;
    this.replaceChildren(...[this.#legend, this.#svg, this.#readout, this.#notice, this.#probe, this.#hint, this.#pop].filter((e) => e !== null));
    this.#listen();
  }

  async #load(): Promise<void> {
    const src = this.getAttribute("src");
    if (!src || src === this.#loadedSrc) return;
    this.#loadedSrc = src;
    try {
      const response = await fetch(new URL(src, document.baseURI));
      if (!response.ok) throw new Error(`${src}: HTTP ${response.status}`);
      const json: unknown = await response.json();
      if (this.#loadedSrc !== src) return;
      this.#base = response.url || new URL(src, document.baseURI).href;
      this.#take(() => parseChart(json));
    } catch (e) {
      if (this.#loadedSrc !== src) return;
      this.#loadedSrc = null;
      this.#fail(e);
    }
  }

  #take(parse: () => ChartData): void {
    // The reader's setting stays picked where the new data has it: a page that slices a table
    // keeps the reader where they were.
    const picked = this.#x === null || !this.#data ? null : this.#data.x.values[this.#x];
    try {
      this.#data = parse();
    } catch (e) {
      this.#fail(e);
      return;
    }
    if (this.#notice) this.#notice.hidden = true;
    this.#x = this.#indexOf(picked) ?? this.#indexOf(this.getAttribute("selected"));
    // A chart that will sweep hides its data until the sweep starts, by CSS only: a reader without
    // the page's CSS, or without scripts past this point, still sees the whole figure.
    if (!this.#swept && this.#sweepWanted()) this.dataset.pending = "";
    this.#draw();
    if (this.#visible) this.#startSweep();
  }

  #fail(e: unknown): void {
    const error = e instanceof Error ? e : new Error(String(e));
    if (this.#notice) {
      this.#notice.textContent = error.message.split("\n")[0];
      this.#notice.hidden = false;
    }
    console.error(error);
    this.dispatchEvent(new CustomEvent("tickwave-error", { bubbles: true, composed: true, detail: { message: error.message, error } }));
  }

  #sweepWanted(): boolean {
    return this.getAttribute("sweep") !== "off" && !reducedMotion() && !this.hasAttribute("highlight") && !matchMedia("print").matches;
  }

  #startSweep(): void {
    if (this.#swept || !this.#data || !this.#layout) return;
    this.#swept = true;
    delete this.dataset.pending;
    if (!this.#sweepWanted()) return this.#finishSweep();
    const start = performance.now();
    const frame = (now: number) => {
      if (this.#sweep === null && this.#labels === null) return; // finished early
      const t = (now - start) / SWEEP_MS;
      if (t < 1) {
        this.#sweep = 1 - (1 - t) ** 3;
      } else {
        this.#sweep = null;
        this.#labels = Math.min(1, (t - 1) * (SWEEP_MS / LABELS_MS));
        if (this.#labels >= 1) return this.#finishSweep();
      }
      this.#draw();
      requestAnimationFrame(frame);
    };
    this.#sweep = 0;
    this.#labels = 0;
    this.#draw();
    requestAnimationFrame(frame);
  }

  #finishSweep(): void {
    this.#swept = true;
    delete this.dataset.pending;
    this.#sweep = null;
    this.#labels = null;
    this.#draw();
    this.#settle();
  }

  /** The series the page shows: `show`, else every series not hidden. */
  #listed(): Set<string> {
    const data = this.#data!;
    const show = this.getAttribute("show");
    return new Set(show === null ? data.series.filter((s) => !s.hidden).map((s) => s.id) : show.split(/\s+/).filter(Boolean));
  }

  #shown(): Set<string> {
    const shown = this.#listed();
    // A hidden series draws while it is picked: the landing page's GCC column.
    if (this.#series) shown.add(this.#series);
    return shown;
  }

  #colour(): Colours {
    if (this.#colours) return this.#colours;
    const probe = this.#probe!;
    const style = getComputedStyle(this);
    const read = (expr: string) => {
      probe.style.color = expr;
      return normalise(getComputedStyle(probe).color) ?? "rgb(0, 0, 0)";
    };
    const token = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
    const cache = new Map<string, string>();
    const dashes: Record<Pattern | "misread", string> = {
      solid: "",
      "second-build": token("--dash-second-build", "6 3"),
      reference: token("--dash-reference", "8 3 2 3"),
      "capture-second": token("--dash-capture-second", "12 4"),
      misread: token("--dash-misread", "2 4"),
    };
    this.#colours = {
      series: (name) => {
        let c = cache.get(name);
        if (c === undefined) cache.set(name, (c = read(`var(--series-${name}, var(--ink, CanvasText))`)));
        return c;
      },
      dash: (p) => dashes[p],
      fault: read("var(--fault-ink, var(--attention-ink, CanvasText))"),
      captureWidth: parseFloat(token("--capture-width", "1.5")) || 1.5,
      captureOpacity: opacity(token("--capture-opacity", "60%")),
      fontText: token("--font-text", "system-ui, sans-serif"),
    };
    return this.#colours;
  }

  #draw(): void {
    const data = this.#data;
    const svg = this.#svg;
    if (!data || !svg || !this.isConnected) return;
    const width = Math.max(240, Math.round(this.clientWidth));
    const height = Number(this.getAttribute("height")) || DEFAULT_HEIGHT;
    const endLabels = this.hasAttribute("end-labels");
    const c = this.#colour();
    const shown = this.#shown();
    const labelWidth = endLabels
      ? Math.max(0, ...data.series.filter((s) => shown.has(s.id)).map((s) => textWidth(s.short, c.fontText)))
      : null;
    const l = (this.#layout = layout(data, shown, width, height, labelWidth));
    svg.setAttribute("width", String(width));
    svg.setAttribute("height", String(height));
    svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
    svg.setAttribute("font-family", c.fontText);
    const sweepX = this.#sweep === null ? null : l.sweepFrom + this.#sweep * (l.sweepTo - l.sweepFrom);
    const markup = render(
      l,
      {
        x: this.#x,
        series: this.#series,
        limit: this.#limit,
        rings: this.#rings(),
        sweep: sweepX,
        labels: this.#sweep !== null ? 0 : this.#labels,
        endLabels: labelWidth !== null,
        xName: [data.x.name, data.x.unit].filter(Boolean).join(", "),
        yUnit: data.y.unit ?? "",
      },
      c,
    );
    svg.innerHTML = `<g class="tw-data">${markup}</g>`;
    // The description says Enter opens a point's recording while any shown point names one.
    this.#name();
    this.#drawLegend(data, c);
    this.#drawReadout(data, c);
    if (!this.#ready) {
      this.#ready = true;
      if (!this.#sweepWanted()) this.#finishSweep();
      this.dispatchEvent(new CustomEvent("tickwave-ready", { bubbles: true, composed: true, detail: { height: this.offsetHeight } }));
    }
  }

  #rings(): [string, number][] {
    const spec = this.getAttribute("highlight");
    if (!spec || !this.#data) return [];
    return spec.split(/\s+/).flatMap((ref) => {
      const [id, x] = ref.split("@");
      const i = this.#indexOf(x ?? null);
      return id && i !== null ? [[id, i] as [string, number]] : [];
    });
  }

  #drawLegend(data: ChartData, c: Colours): void {
    const ul = this.#legend;
    if (!ul) return;
    const listed = this.#listed();
    const items = data.series
      .filter((s) => listed.has(s.id))
      .map((s) => `<li tabindex="0" data-series="${escapeHtml(s.id)}">${swatch(s, c)}${escapeHtml(s.name)}</li>`);
    if (data.reference) {
      items.push(`<li data-reference=""><svg class="tw-swatch" width="22" height="10" aria-hidden="true"><line x1="1" x2="21" y1="9" y2="1" stroke="currentColor" stroke-opacity=".5" stroke-dasharray="4 4"/></svg>as asked</li>`);
    }
    data.limits.forEach((m, i) => {
      items.push(`<li tabindex="0" data-limit="${i}"><svg class="tw-swatch" width="22" height="10" aria-hidden="true"><line x1="11" x2="11" y1="0" y2="10" stroke="currentColor" stroke-width="1.5" stroke-dasharray="3 2"/></svg>${escapeHtml(m.label)}</li>`);
    });
    const html = items.join("");
    if (html !== this.#legendHtml) ul.innerHTML = this.#legendHtml = html;
  }

  #drawReadout(data: ChartData, c: Colours): void {
    const box = this.#readout!;
    const tooltip = this.getAttribute("readout") !== "below";
    box.toggleAttribute("data-tooltip", tooltip);
    const i = this.#x;
    if (i === null || !this.#layout) {
      box.textContent = this.#readoutHtml = "";
      return;
    }
    const shown = this.#shown();
    const unit = data.y.unit ?? "";
    const rows = data.series
      .filter((s) => shown.has(s.id))
      .map((s) => `<div>${swatch(s, c)}${escapeHtml(tooltip ? s.short : s.name)} ${escapeHtml(figure(s.points[i], unit))}</div>`);
    const head = `<b>${escapeHtml([grouped(data.x.values[i]), data.x.unit].filter(Boolean).join(" "))}</b>`;
    const html = head + rows.join("");
    if (html !== this.#readoutHtml) box.innerHTML = this.#readoutHtml = html;
    if (!tooltip) {
      box.style.left = box.style.top = "";
      return;
    }
    // Beside the picked setting, to its left near the right edge; at the plot's top, or lower where
    // that hides less of the lines.
    const l = this.#layout;
    const host = this.getBoundingClientRect();
    const plot = this.#svg!.getBoundingClientRect();
    const dx = plot.left - host.left;
    const w = box.offsetWidth;
    const h = box.offsetHeight;
    const left = dx + l.xs[i] + 14 + w > this.clientWidth ? l.xs[i] - 14 - w : l.xs[i] + 14;
    const tops = [0, 1, 2, 3, 4].map((k) => l.top + (k / 4) * Math.max(0, l.bottom - h - l.top));
    const scores = tops.map((y) => hidden(l, left, y, w, h));
    const top = tops[scores.indexOf(Math.min(...scores))];
    box.style.left = `${dx + left}px`;
    box.style.top = `${plot.top - host.top + top}px`;
  }

  #listen(): void {
    const svg = this.#svg!;
    const at = (e: PointerEvent): number | null => {
      const l = this.#layout;
      if (!l) return null;
      const r = svg.getBoundingClientRect();
      const x = e.clientX - r.left;
      const y = e.clientY - r.top;
      if (y < l.top - 8 || y > l.bottom + 24 || x < l.left - 12 || x > l.right + 12) return null;
      const i = l.bands.findIndex(([a, b]) => x >= a && x < b);
      return i < 0 ? (x < l.left ? 0 : l.xs.length - 1) : i;
    };
    const endOf = (e: Event) => (e.target as Element).closest?.(".tw-end")?.parentElement?.dataset.series ?? null;
    /** The nearest target within reach of the pointer. Of points drawn on one another, the picked
     * series' one, else the first in the data, as the keys choose. */
    const near = (e: PointerEvent | MouseEvent): Pointed | null => {
      const r = svg.getBoundingClientRect();
      const x = e.clientX - r.left;
      const y = e.clientY - r.top;
      const within = this.#targets()
        .map((p) => ({ p, d: Math.hypot(p.at.x - x, p.at.y - y) }))
        .filter((c) => c.d <= REACH);
      const nearest = Math.min(...within.map((c) => c.d));
      const ties = within.filter((c) => c.d < nearest + 1).map((c) => c.p);
      return ties.find((p) => p.series.series.id === this.#series) ?? ties[0] ?? null;
    };
    svg.addEventListener("pointermove", (e) => {
      if (e.pointerType !== "mouse") return;
      // An end label names its series while the pointer is on it.
      const label = endOf(e);
      let series = this.#series;
      if (label) {
        series = label;
        this.#fromLabel = true;
      } else if (this.#fromLabel) {
        series = null;
        this.#fromLabel = false;
      }
      this.#pick(at(e) ?? this.#x, series);
      const pointed = near(e);
      svg.style.cursor = pointed ? "pointer" : "";
      if (!pointed) this.#closeEvidence();
      else if (!samePoint(pointed, this.#pointed) && !samePoint(pointed, this.#resting)) {
        // The popover belongs to the point under the pointer: another point's closes, and this
        // one opens once the mouse has rested on it.
        this.#closeEvidence();
        this.#resting = pointed;
        this.#dwell = window.setTimeout(() => {
          this.#resting = null;
          void this.#openEvidence(pointed);
        }, DWELL_MS);
      }
    });
    svg.addEventListener("pointerdown", (e) => {
      const i = at(e);
      if (i !== null) this.#pick(i, endOf(e) ?? this.#series);
      const pointed = near(e);
      if (pointed) void this.#openEvidence(pointed);
    });
    svg.addEventListener("click", (e) => {
      const pointed = near(e);
      if (pointed) void this.#goToEvidence(pointed);
    });
    svg.addEventListener("pointerleave", (e) => {
      if (e.pointerType !== "mouse") return;
      this.#fromLabel = false;
      svg.style.cursor = "";
      this.#closeEvidence();
      if (!this.#sticky) this.#pick(null, null);
      else if (this.#series) this.#pick(this.#x, null);
    });
    const legendItem = (e: Event) => (e.target as Element).closest?.("li") as HTMLLIElement | null;
    const enter = (e: Event) => {
      const li = legendItem(e);
      if (!li || li.dataset.reference !== undefined) return;
      if (li.dataset.limit !== undefined) {
        this.#limit = true;
        this.#draw();
      } else this.#pick(this.#x, li.dataset.series ?? null);
    };
    const leave = (e: Event) => {
      const li = legendItem(e);
      if (!li || li.dataset.reference !== undefined) return;
      this.#limit = false;
      if (li.dataset.series !== undefined) this.#pick(this.#x, null);
      else this.#draw();
    };
    this.#legend?.addEventListener("pointerover", enter);
    this.#legend?.addEventListener("pointerout", leave);
    this.#legend?.addEventListener("focusin", enter);
    this.#legend?.addEventListener("focusout", leave);
  }

  #key(e: KeyboardEvent): void {
    if (!this.#data || e.target !== this) return;
    const n = this.#data.x.values.length;
    const i = this.#x;
    let next: number | null | undefined;
    if (e.key === "ArrowRight") next = i === null ? 0 : Math.min(n - 1, i + 1);
    else if (e.key === "ArrowLeft") next = i === null ? n - 1 : Math.max(0, i - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    else if (e.key === "Escape" && !this.#sticky) next = null;
    else if (e.key === "Enter" && this.#pointed) {
      e.preventDefault();
      void this.#goToEvidence(this.#pointed);
      return;
    }
    if (next === undefined) return;
    e.preventDefault();
    this.#pick(next, next === null ? null : this.#series);
    // Reaching a setting with the keys opens its evidence: the picked series' point there, or else
    // the first shown point there that names a recording.
    const pointed = next === null ? null : this.#evidenceAt(next);
    if (pointed) void this.#openEvidence(pointed);
    else this.#closeEvidence();
  }

  #evidenceAt(i: number): Pointed | null {
    const all = this.#targets().filter((p) => p.at.index === i);
    return all.find((p) => p.series.series.id === this.#series) ?? all[0] ?? null;
  }

  /** What a reader can point at to open a recording: the shown points that name one, then the
   * failures' marks whose points name one, in drawing order. A failure without a mark has nothing to
   * point at, so its recording stays closed to the pointer and the keys alike. */
  #targets(): Pointed[] {
    const l = this.#layout;
    if (!l) return [];
    const out: Pointed[] = [];
    for (const series of l.series) {
      for (const at of series.placed) if (at.point.evidence) out.push({ series, at, evidence: at.point.evidence });
    }
    for (const mark of l.fails) {
      const series = l.series.find((s) => s.series.id === mark.series);
      if (series && mark.point.evidence) out.push({ series, at: mark, evidence: mark.point.evidence });
    }
    return out;
  }

  #url(relative: string): string {
    return new URL(relative, this.#base).href;
  }

  /** Shows a point's recording at its annotation in a popover beside the point. */
  async #openEvidence(pointed: Pointed): Promise<void> {
    const same = (a: Pointed | null) => samePoint(a, pointed);
    if (same(this.#pointed)) return;
    this.#pointed = pointed;
    try {
      await loadElements();
    } catch (e) {
      this.#fail(e);
      return;
    }
    if (!same(this.#pointed) || !this.#pop || !this.#svg) return;
    const pop = this.#pop;
    const inset = (this.#inset ??= pop.appendChild(document.createElement("tickwave-inset")));
    const { evidence } = pointed;
    const set = (name: string, value: string | null) => {
      if (value === null) inset.removeAttribute(name);
      else if (inset.getAttribute(name) !== value) inset.setAttribute(name, value);
    };
    set("src", this.#url(evidence.recording));
    set("annotations", evidence.annotations === null ? null : this.#url(evidence.annotations));
    set("annotation", evidence.annotation);
    // The probe's lines take the series' hue, so the inset's key tells probe from target.
    set("series", pointed.series.series.colour || null);
    const head = `${swatch(pointed.series.series, this.#colour())}${escapeHtml(pointed.series.series.name)}`;
    if (this.#popHead && this.#popHeadHtml !== head) this.#popHead.innerHTML = this.#popHeadHtml = head;
    if (!pop.matches(":popover-open")) pop.showPopover();
    this.#stopPosition?.();
    const svg = this.#svg;
    // Below the chart, or above it, at the point's x as it is drawn now: never over the lines, the
    // legend or the readout. The picked setting's rule joins the point to it.
    const anchor = {
      contextElement: svg,
      getBoundingClientRect: () => {
        const plot = svg.getBoundingClientRect();
        const chart = this.getBoundingClientRect();
        const at = this.#targets().find((p) => samePoint(p, pointed))?.at ?? pointed.at;
        return new DOMRect(plot.left + at.x - 5, chart.top, 10, chart.height);
      },
    };
    const place = () =>
      computePosition(anchor, pop, { strategy: "fixed", placement: "bottom", middleware: [offset(8), flip({ padding: 8 }), shift({ padding: 8 })] }).then(({ x, y }) => {
        pop.style.left = `${x}px`;
        pop.style.top = `${y}px`;
      });
    this.#stopPosition = autoUpdate(anchor, pop, place);
  }

  #closeEvidence(): void {
    window.clearTimeout(this.#dwell);
    this.#resting = null;
    this.#pointed = null;
    this.#stopPosition?.();
    this.#stopPosition = null;
    if (this.#pop?.matches(":popover-open")) this.#pop.hidePopover();
  }

  /** Tells the page a reader asked for a point's recording. Unless it takes over, opens the page's
   * view of that recording at the annotation; with no such view, the popover stays. */
  async #goToEvidence(pointed: Pointed): Promise<void> {
    const { evidence } = pointed;
    const recording = this.#url(evidence.recording);
    const detail = {
      x: this.#data!.x.values[pointed.at.index],
      series: pointed.series.series.id,
      recording,
      annotations: evidence.annotations === null ? null : this.#url(evidence.annotations),
      annotation: evidence.annotation,
    };
    const go = this.dispatchEvent(new CustomEvent("tickwave-evidence", { bubbles: true, composed: true, cancelable: true, detail }));
    const view = [...document.querySelectorAll("tickwave-view")].find((v) => {
      const src = v.getAttribute("src");
      return src !== null && new URL(src, document.baseURI).href === recording;
    }) as (HTMLElement & { reveal(id: string): Promise<void> }) | undefined;
    if (go && !view) return;
    this.#closeEvidence();
    if (!go || !view) return;
    await view.reveal(evidence.annotation);
  }

  /** A reader's pick: draws, tells the page, and moves the group's other charts. */
  #pick(i: number | null, series: string | null): void {
    if (i === this.#x && series === this.#series) return;
    const moved = i !== this.#x;
    this.#x = i;
    this.#series = series;
    this.#draw();
    const x = i === null ? null : this.#data!.x.values[i];
    if (moved) this.#moveGroup();
    this.dispatchEvent(new CustomEvent("tickwave-pick", { bubbles: true, composed: true, detail: { x, series } }));
  }

  /** The group's other charts take this one's setting. */
  #moveGroup(): void {
    const x = this.#x === null ? null : this.#data!.x.values[this.#x];
    for (const other of groups.get(this.getAttribute("group") ?? "") ?? []) {
      if (other !== this) other.#follow(x);
    }
  }

  #follow(x: string | null): void {
    const i = this.#indexOf(x);
    if (i === null && x !== null) return;
    if (i === null && this.#sticky) return;
    this.#x = i;
    this.#draw();
  }

  #clear(): void {
    if (this.#sticky || (this.#x === null && this.#series === null)) return;
    this.#pick(null, null);
  }

  #join(group: string | null): void {
    for (const [name, set] of groups) {
      set.delete(this);
      if (!set.size) groups.delete(name);
    }
    if (!group) return;
    let set = groups.get(group);
    if (!set) groups.set(group, (set = new Set()));
    set.add(this);
  }
}

let measure: CanvasRenderingContext2D | null | undefined;
/** An end label's width in CSS pixels, in the weight and size the plot draws it. */
function textWidth(text: string, font: string): number {
  measure ??= document.createElement("canvas").getContext("2d");
  if (!measure) return text.length * 7;
  measure.font = `600 12px ${font}`;
  return Math.ceil(measure.measureText(text).width);
}

/** How much of the shown points and lines a box over the plot would hide, in plot coordinates: a
 * point counts as much as half a segment. */
function hidden(l: Layout, x: number, y: number, w: number, h: number): number {
  const inside = (px: number, py: number) => px >= x && px <= x + w && py >= y && py <= y + h;
  let n = 0;
  for (const s of l.series) {
    for (const p of s.placed) if (inside(p.x, p.y)) n += 4;
    for (const { from, to } of s.segments) {
      for (let k = 1; k < 8; k++) if (inside(from.x + (k / 8) * (to.x - from.x), from.y + (k / 8) * (to.y - from.y))) n++;
    }
  }
  return n;
}

function samePoint(a: Pointed | null, b: Pointed | null): boolean {
  return a !== null && b !== null && a.series.series.id === b.series.series.id && a.at.index === b.at.index;
}

/** A CSS opacity, `60%` or `0.6`, as a fraction. */
function opacity(css: string): number {
  const v = parseFloat(css);
  if (!Number.isFinite(v)) return 0.6;
  return css.includes("%") ? v / 100 : v;
}

