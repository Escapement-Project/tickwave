// <tickwave-chart>: measurements against a measured setting (CHARTS.md). It draws into its own
// children, not a shadow root, so reader view and feeds keep the figure.

import { onFaceChange, onFontsLoaded, reducedMotion } from "../face.ts";
import { normalise } from "../palette.ts";
import { parseChart, type ChartData, type Pattern } from "./data.ts";
import { grouped } from "./decimal.ts";
import { layout, type Layout } from "./layout.ts";
import { figure, swatch } from "./readout.ts";
import { render, type Colours } from "./render.ts";

const SVG = "http://www.w3.org/2000/svg";
const SWEEP_MS = 750;
const LABELS_MS = 200;
const DEFAULT_HEIGHT = 330;

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
`;

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
  #connected = false;

  constructor() {
    super();
    this.addEventListener("keydown", (e) => this.#key(e));
  }

  connectedCallback(): void {
    this.#connected = true;
    addStyle();
    if (!this.hasAttribute("tabindex")) this.tabIndex = 0;
    this.setAttribute("role", "group");
    this.setAttribute("aria-roledescription", "chart");
    this.#name();
    this.#build();
    const resize = new ResizeObserver(() => this.#draw());
    resize.observe(this);
    const seen = new IntersectionObserver((entries) => {
      this.#visible = entries.some((e) => e.isIntersecting);
      if (this.#visible) this.#startSweep();
    }, { threshold: 0.6 });
    seen.observe(this);
    // A press in any chart of the group is a pick there, which moves this one. The path, not the
    // target: a pick redraws the plot and detaches the target before this runs.
    const outside = (e: PointerEvent) => {
      const path = e.composedPath();
      const group = groups.get(this.getAttribute("group") ?? "") ?? [this];
      if (![...group].some((chart) => path.includes(chart))) this.#clear();
    };
    const print = () => this.#finishSweep();
    document.addEventListener("pointerdown", outside);
    addEventListener("beforeprint", print);
    this.#cleanups.push(
      () => resize.disconnect(),
      () => seen.disconnect(),
      () => document.removeEventListener("pointerdown", outside),
      () => removeEventListener("beforeprint", print),
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
    this.#take(() => parseChart(value));
  }

  /** Selects a setting, a series or both, as a reader would; `pick()` clears. Fires no event. */
  pick(x: string | null = null, series: string | null = null): void {
    this.#x = x === null ? (this.#sticky ? this.#x : null) : this.#indexOf(x);
    this.#series = series;
    this.#limit = false;
    this.#draw();
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
    const table = this.getAttribute("table");
    if (table) this.setAttribute("aria-describedby", table);
    else this.removeAttribute("aria-describedby");
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
    this.replaceChildren(...[this.#legend, this.#svg, this.#readout, this.#notice, this.#probe].filter((e) => e !== null));
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
      if (this.#loadedSrc === src) this.#take(() => parseChart(json));
    } catch (e) {
      this.#fail(e);
    }
  }

  #take(parse: () => ChartData): void {
    try {
      this.#data = parse();
    } catch (e) {
      this.#fail(e);
      return;
    }
    if (this.#notice) this.#notice.hidden = true;
    this.#x = this.#indexOf(this.getAttribute("selected"));
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

  #shown(): Set<string> {
    const data = this.#data!;
    const show = this.getAttribute("show");
    const ids = show === null ? data.series.filter((s) => !s.hidden).map((s) => s.id) : show.split(/\s+/).filter(Boolean);
    const shown = new Set(ids);
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
    const l = (this.#layout = layout(data, this.#shown(), width, height, endLabels));
    const c = this.#colour();
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
        endLabels,
        xName: [data.x.name, data.x.unit].filter(Boolean).join(", "),
        yUnit: data.y.unit ?? "",
      },
      c,
    );
    svg.innerHTML = `<g class="tw-data">${markup}</g>`;
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
    const shown = this.#shown();
    const items = data.series
      .filter((s) => shown.has(s.id) && !s.hidden)
      .map((s) => `<li tabindex="0" data-series="${s.id}">${swatch(s, c)}${escapeHtml(s.name)}</li>`);
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
    // Beside the picked setting, off the lines; to its left near the right edge.
    const l = this.#layout;
    const host = this.getBoundingClientRect();
    const plot = this.#svg!.getBoundingClientRect();
    const x = plot.left - host.left + l.xs[i];
    const w = box.offsetWidth;
    box.style.left = `${x + 14 + w > this.clientWidth ? x - 14 - w : x + 14}px`;
    box.style.top = `${plot.top - host.top + l.top}px`;
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
    });
    svg.addEventListener("pointerdown", (e) => {
      const i = at(e);
      if (i !== null) this.#pick(i, endOf(e) ?? this.#series);
    });
    svg.addEventListener("pointerleave", (e) => {
      if (e.pointerType !== "mouse") return;
      this.#fromLabel = false;
      if (!this.#sticky) this.#pick(null, null);
      else if (this.#series) this.#pick(this.#x, null);
    });
    const legendItem = (e: Event) => (e.target as Element).closest?.("li") as HTMLLIElement | null;
    const enter = (e: Event) => {
      const li = legendItem(e);
      if (!li) return;
      if (li.dataset.limit !== undefined) {
        this.#limit = true;
        this.#draw();
      } else this.#pick(this.#x, li.dataset.series ?? null);
    };
    const leave = (e: Event) => {
      const li = legendItem(e);
      if (!li) return;
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
    if (next === undefined) return;
    e.preventDefault();
    this.#pick(next, next === null ? null : this.#series);
  }

  /** A reader's pick: draws, tells the page, and moves the group's other charts. */
  #pick(i: number | null, series: string | null): void {
    if (i === this.#x && series === this.#series) return;
    const moved = i !== this.#x;
    this.#x = i;
    this.#series = series;
    this.#draw();
    const x = i === null ? null : this.#data!.x.values[i];
    if (moved) {
      for (const other of groups.get(this.getAttribute("group") ?? "") ?? []) {
        if (other !== this) other.#follow(x);
      }
    }
    this.dispatchEvent(new CustomEvent("tickwave-pick", { bubbles: true, composed: true, detail: { x, series } }));
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

/** A CSS opacity, `60%` or `0.6`, as a fraction. */
function opacity(css: string): number {
  const v = parseFloat(css);
  if (!Number.isFinite(v)) return 0.6;
  return css.includes("%") ? v / 100 : v;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[ch]!);
}
