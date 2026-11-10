// <tickwave-view>: the full capture. Collapsed to one line until `open`; open, a time axis, one row
// a channel and an annotation lane, with zoom, pan, cursors, a selection and its readouts.

import { ofKind, type Annotation } from "../annotations.ts";
import { parseChannels, parseWindow, VIEW_ATTRIBUTES } from "../attributes.ts";
import { reducedMotion } from "../face.ts";
import { absolute, events as registryEvents, infoFor, peekInfo } from "../registry.ts";
import { clockNote, countEdges, formatBrief, formatBytes, formatClockPeriod, formatClockRate, formatCount, formatInterval, formatRate, formatValue, periodStats } from "../readouts.ts";
import { lowerBound } from "../search.ts";
import { channelIndices, Plot } from "../render/plot.ts";
import { isAbort, named, sampleAtOrBefore, sampleTick, type RecordingInfo } from "../source.ts";
import { parseTime, toTicks, type TimeSpec } from "../time.ts";
import { TickwaveElement, loadRecording, type Loaded } from "./base.ts";

const CSS = `
.box { background: var(--surface, Canvas); border: 1px solid var(--hair, GrayText); border-radius: 10px; }
.summary { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px; padding: 8px 10px 8px 14px; }
.summary .title { font-weight: 600; }
.summary .facts { color: var(--ink-2, GrayText); font-size: 13px; flex: 1 1 12em; }
.viewer { display: flex; flex-direction: column; height: 100%; box-sizing: border-box; padding: 6px 0 8px; }
.viewer[hidden], .summary[hidden] { display: none; }
.toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 0 10px 6px 14px; }
.toolbar .title { font-weight: 600; margin-right: auto; }
.toolbar .group { display: flex; gap: 4px; align-items: center; }
.plot { position: relative; flex: 1 1 auto; min-height: 0; touch-action: pan-y; cursor: crosshair; }
.plot:focus-visible { outline-offset: -2px; border-radius: 4px; }
.plot.panning { cursor: grabbing; }
canvas { display: block; }
.readout { display: grid; gap: 3px; padding: 6px 14px 0; font-size: 13px; color: var(--ink-2, GrayText); }
.readout:empty { display: none; }
.readout .line { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 14px; }
.readout .figures { color: var(--ink, CanvasText); }
.readout label { display: inline-flex; gap: 6px; align-items: baseline; }
.readout select { min-height: 22px; padding: 1px 4px; font-size: 12px; }
.steps { display: flex; gap: 4px; align-items: center; }
.steps .count { min-width: 5.5em; text-align: center; color: var(--ink-2, GrayText); font-size: 13px; }
.steps[hidden] { display: none; }
.hint { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); pointer-events: none;
  padding: 6px 12px; border-radius: 6px; background: var(--ink, CanvasText); color: var(--page, Canvas);
  font-size: 13px; white-space: nowrap; opacity: 0; transition: opacity .15s; }
.hint.shown { opacity: 1; }
@media (prefers-reduced-motion: reduce) { .hint { transition: none; } }
`;

const HTML = `
<div class="summary box" part="summary">
  <span class="title" part="title"></span><span class="facts" part="facts"></span>
  <button type="button" class="open" part="button open-button">Open</button>
</div>
<div class="viewer box" part="viewer" hidden>
  <div class="toolbar" part="toolbar">
    <span class="title" part="title"></span>
    <div class="steps" part="steps" hidden>
      <select class="kind" aria-label="Annotation kind"></select>
      <button type="button" class="prev" part="button" aria-label="Previous">‹</button>
      <span class="count figures" aria-live="polite"></span>
      <button type="button" class="next" part="button" aria-label="Next">›</button>
    </div>
    <div class="group">
      <button type="button" class="zoom-out" part="button" aria-label="Zoom out">−</button>
      <button type="button" class="zoom-in" part="button" aria-label="Zoom in">+</button>
      <button type="button" class="fit" part="button">Fit</button>
      <button type="button" class="close" part="button">Close</button>
    </div>
  </div>
  <div class="plot" part="plot" tabindex="0" role="group" aria-roledescription="capture"><canvas aria-hidden="true"></canvas><div class="hint" part="hint" aria-hidden="true"></div></div>
  <div class="readout" part="readout" aria-live="polite"></div>
</div>
<div class="notice" part="notice" role="status" hidden></div>`;

const DRAG_PX = 4;
const SNAP_PX = 6;
/** The narrowest view: this many CSS pixels a tick. */
const MAX_PX_PER_TICK = 40;

interface Drag {
  readonly id: number;
  readonly x: number;
  readonly y: number;
  readonly area: "row" | "axis" | "lane" | "none";
  readonly row: number;
  readonly view: { start: number; end: number };
  moved: boolean;
}

export class TickwaveView extends TickwaveElement {
  static observedAttributes = VIEW_ATTRIBUTES;
  readonly #plot: Plot;
  readonly #summary: HTMLElement;
  readonly #viewer: HTMLElement;
  readonly #plotBox: HTMLElement;
  readonly #readout: HTMLElement;
  readonly #steps: HTMLElement;
  readonly #kind: HTMLSelectElement;
  #loaded: Loaded | null = null;
  #loading: Promise<Loaded | null> | null = null;
  #loadKey = "";
  #recordingKey = "";
  /** The recording drawn, whose cursors and selection the plot holds. */
  #shownSrc = "";
  /** A window asked for before the head arrived, applied once it has. */
  #pendingWindow: [number | TimeSpec, number | TimeSpec] | null = null;
  /** Whether the view is on screen: a collapsed one fetches its head then. */
  #inView = false;
  #size = { width: 0, height: 0 };
  #wasOpen = false;
  #windowAttr: string | null = null;
  #pointers = new Map<number, { x: number; y: number }>();
  #drag: Drag | null = null;
  #pinch: { distance: number; mid: number; view: { start: number; end: number } } | null = null;
  #nextCursor: "a" | "b" = "a";
  #periodChannel = -1;
  #readoutSeq = 0;
  #stepIndex = -1;
  #windowEventQueued = false;
  #readyFired = false;
  #resize: ResizeObserver;
  /** A collapsed view fetches its recording's head, and only that, once it scrolls into view. */
  #seen = new IntersectionObserver((entries) => {
    this.#inView = entries.some((e) => e.isIntersecting);
    this.#fetchHead();
  });
  #onHead = (e: Event) => {
    if ((e as CustomEvent<string>).detail === this.#src?.href) this.#renderSummary();
  };

  constructor() {
    super(CSS, HTML);
    this.#summary = this.root.querySelector(".summary")!;
    this.#viewer = this.root.querySelector(".viewer")!;
    this.#plotBox = this.root.querySelector(".plot")!;
    this.#readout = this.root.querySelector(".readout")!;
    this.#steps = this.root.querySelector(".steps")!;
    this.#kind = this.root.querySelector(".kind")!;
    this.#plot = new Plot(this.root.querySelector("canvas")!, {
      axis: true,
      lanes: 3,
      rowHeight: 28,
      analogHeight: 72,
      noteRules: false,
      labelPx: 12,
    });
    this.#plot.onError = (e) => this.fail(e);
    this.#plot.onRecovered = (m) => this.forgetProblem(m);
    this.#plot.onStall = (ms) => this.waiting(ms);
    this.#plot.onDraw = () => this.#drawn();
    this.#resize = new ResizeObserver(() => this.#sized());
    const on = (sel: string, f: () => void) => this.root.querySelector(sel)!.addEventListener("click", f);
    // The pressed button hides: the focus goes where the reader went, not to the page's start.
    on(".open", () => {
      this.open = true;
      this.update();
      this.#plotBox.focus();
    });
    on(".close", () => {
      this.open = false;
      this.update();
      (this.root.querySelector(".open") as HTMLElement).focus();
    });
    on(".zoom-in", () => this.#zoomBy(1 / 1.6, null));
    on(".zoom-out", () => this.#zoomBy(1.6, null));
    on(".fit", () => this.#fit());
    on(".prev", () => this.#step(-1));
    on(".next", () => this.#step(1));
    this.#kind.addEventListener("change", () => {
      this.#stepIndex = -1;
      this.#renderSteps();
    });
    this.#listen();
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.#resize.observe(this.#plotBox);
    this.#seen.observe(this);
    registryEvents.addEventListener("tickwave-head", this.#onHead);
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.#resize.disconnect();
    this.#seen.disconnect();
    registryEvents.removeEventListener("tickwave-head", this.#onHead);
  }

  get #src(): URL | null {
    const s = this.getAttribute("src");
    return s ? absolute(s) : null;
  }

  get open(): boolean {
    return this.hasAttribute("open");
  }

  set open(v: boolean) {
    this.toggleAttribute("open", v);
  }

  /** The visible span in ticks. */
  get window(): { start: number; end: number } {
    return { ...this.#plot.view };
  }

  protected paletteChanged(): void {
    this.#plot.palette = this.palette;
    if (this.#loaded) this.#layout(true);
  }

  protected update(): void {
    const open = this.open;
    this.#summary.hidden = open;
    this.#viewer.hidden = !open;
    if (open && !this.#wasOpen) this.emit("tickwave-open", { src: this.getAttribute("src") });
    this.#wasOpen = open;
    if (!open) {
      this.#fetchHead();
      this.#renderSummary();
      return;
    }
    const src = this.getAttribute("src");
    if (!src) return;
    const annotations = this.getAttribute("annotations");
    const key = `${src}\n${annotations ?? ""}`;
    if (key !== this.#loadKey) {
      this.#loadKey = key;
      this.#loaded = null;
      this.#readyFired = false;
      this.clearProblems();
      this.#loading = loadRecording(src, annotations).then(
        (l) => {
          if (this.#loadKey !== key) return null;
          this.#loaded = l;
          for (const e of l.annotations.errors) this.fail(named("TickwaveSidecarError", e));
          this.#windowAttr = null;
          this.#apply();
          return l;
        },
        (e) => {
          // Not kept: the next update, an Open or a show(), tries again.
          if (this.#loadKey === key) this.#loadKey = "";
          this.fail(e);
          return null;
        },
      );
      return;
    }
    this.#apply();
  }

  /** A collapsed view on screen fetches its recording's head, and only that. */
  #fetchHead(): void {
    const src = this.#src;
    if (src && this.#inView && !this.open) infoFor(src).catch((e) => this.fail(e));
  }

  #renderSummary(): void {
    const src = this.#src;
    const title = this.#summary.querySelector(".title")!;
    const facts = this.#summary.querySelector(".facts")!;
    if (!src) return;
    // Collapsed, the view shows what is here; the head arrives once the view scrolls into sight.
    const info = peekInfo(src);
    title.textContent = info?.title ?? decodeURIComponent(src.pathname.split("/").pop() ?? "");
    // A page with several views has several Open buttons: each names its recording.
    this.root.querySelector(".open")!.setAttribute("aria-label", `Open ${title.textContent}`);
    const parts = (info ? summaryFacts(info) : []).flatMap(([name, text], i) => [
      ...(i > 0 ? [part("fact-separator", "; ")] : []),
      part(`fact ${name}`, text),
    ]);
    facts.replaceChildren(...parts);
  }

  #apply(): void {
    const l = this.#loaded;
    if (!l) return;
    (this.root.querySelector(".toolbar .title") as HTMLElement).textContent = l.info.title;
    this.root.querySelector(".close")!.setAttribute("aria-label", `Close ${l.info.title}`);
    this.#plotBox.setAttribute(
      "aria-label",
      `${l.info.title}. Arrow keys pan, plus and minus zoom, Home fits the capture; click to place cursors, drag on the time axis to select.`,
    );
    const { indices, missing } = channelIndices(l.info, parseChannels(this.getAttribute("channels")));
    for (const m of missing) this.fail(new Error(`no channel ${m}`));
    const recordingKey = `${l.src.href}\n${indices.join(" ")}`;
    const fresh = recordingKey !== this.#recordingKey;
    if (fresh) {
      this.#recordingKey = recordingKey;
      this.#plot.setRecording(l.source, l.info, indices);
      this.#plot.view = { start: 0, end: l.info.length };
      this.#periodChannel = this.#plot.tracks.findIndex((t) => t.channel.kind === "digital");
      if (l.src.href !== this.#shownSrc) {
        this.#shownSrc = l.src.href;
        this.#forget();
      }
    }
    // Another sidecar for the same recording: its lane, its paint and its steps.
    const annotated = this.#plot.annotations !== l.annotations;
    if (annotated) {
      this.#plot.annotations = l.annotations;
      this.#plot.focus = null;
      this.#stepIndex = -1;
    }
    this.#renderKinds();
    this.#layout(fresh || annotated);
    const w = this.getAttribute("window");
    if (w !== this.#windowAttr) {
      this.#windowAttr = w;
      const spec = parseWindow(w);
      if (spec?.kind === "annotation") this.show(spec.id);
      else if (spec) this.setWindow(toTicks(spec.start, l.info.timebase), toTicks(spec.end, l.info.timebase));
    }
    const pending = this.#pendingWindow;
    if (pending) this.#showTicks(pending[0], pending[1]);
  }

  /** A new recording starts clean: the last one's cursors, selection and place among its
   * annotations mean nothing in it. */
  #forget(): void {
    this.#plot.cursors = { a: null, b: null };
    this.#plot.selection = null;
    this.#plot.focus = null;
    this.#nextCursor = "a";
    this.#stepIndex = -1;
    this.#readoutSeq++;
    this.#setReadout([]);
  }

  #sized(): void {
    const width = this.#plotBox.clientWidth;
    const height = this.#plotBox.clientHeight;
    if (width === this.#size.width && Math.abs(height - this.#size.height) < 1) return;
    this.#size = { width, height };
    this.#layout(true);
  }

  #layout(force: boolean): void {
    if (!this.#loaded || !this.palette || this.#size.width <= 0) return;
    if (!force && this.#plot.layout) return;
    // The plot box's height with the canvas collapsed: the page's, when it gives the element one,
    // else nothing, as the box only wraps the canvas. Neither the box with the canvas in it nor the
    // last layout tells the two apart: a readout appearing shrinks a given box below the canvas.
    const canvas = this.#plot.canvas;
    const height = canvas.style.height;
    canvas.style.height = "0px";
    const box = this.#plotBox.clientHeight;
    canvas.style.height = height;
    const given = box > 1 ? box : null;
    this.#plot.resize(this.#size.width, given);
    this.#clampView();
    this.#plot.invalidate();
  }

  // Window ---------------------------------------------------------------------------------------

  #minSpan(): number {
    const l = this.#plot.layout;
    return l ? Math.max(2, (l.plotRight - l.plotLeft) / MAX_PX_PER_TICK) : 2;
  }

  #clampView(): void {
    const info = this.#loaded?.info;
    if (!info) return;
    let { start, end } = this.#plot.view;
    const span = Math.min(info.length, Math.max(this.#minSpan(), end - start));
    const mid = (start + end) / 2;
    start = Math.min(Math.max(0, mid - span / 2), info.length - span);
    end = start + span;
    this.#plot.view = { start, end };
  }

  /** Shows ticks [start, end); numbers are ticks, strings times ("317.8 ms"). Asked before the
   * recording's head has arrived, it shows them once it has. */
  setWindow(start: number | string, end: number | string): void {
    const time = (v: number | string) => {
      if (typeof v === "number") return v;
      const t = parseTime(v);
      if (!t) throw new RangeError(`cannot read the time ${v}`);
      return t;
    };
    this.#showTicks(time(start), time(end));
  }

  #showTicks(start: number | TimeSpec, end: number | TimeSpec): void {
    const info = this.#loaded?.info;
    if (!info) {
      this.#pendingWindow = [start, end];
      return;
    }
    this.#pendingWindow = null;
    const ticks = (v: number | TimeSpec) => (typeof v === "number" ? v : toTicks(v, info.timebase));
    const s = ticks(start);
    const e = ticks(end);
    this.#plot.view = { start: Math.min(s, e), end: Math.max(s, e) };
    this.#viewChanged();
  }

  /** Opens the view at an annotation. Resolves once it is drawn. */
  async show(id: string): Promise<void> {
    if (!this.open) this.open = true;
    this.update();
    const loaded = this.#loaded ?? (await this.#loading);
    if (!loaded || loaded !== this.#loaded) return;
    const a = loaded.annotations.byId.get(id);
    if (!a) {
      this.fail(new Error(`no annotation ${id}`));
      return;
    }
    this.#plot.focus = a.id;
    const span = a.end === null ? 0 : a.end - a.start;
    const pad = span > 0 ? span * 0.25 : Math.max(this.#minSpan() * 10, loaded.info.length / 2000);
    this.#plot.view = { start: a.start - pad, end: (a.end ?? a.start) + pad };
    if (a.kind !== null && this.#kind.value !== a.kind) {
      this.#kind.value = a.kind;
    }
    this.#stepIndex = this.#ofKind().findIndex((b) => b.id === a.id);
    this.#renderSteps();
    this.#viewChanged();
    await this.#plot.whenSettled();
  }

  /** Opens the view at an annotation, brings it into sight and puts the focus on its plot: what a
   * reference or a chart's point does when the reader asks for the recording. */
  /** The focus goes to the plot, where the keys work, or to the Open button while the view is closed. */
  override focus(options?: FocusOptions): void {
    (this.open ? this.#plotBox : (this.root.querySelector(".open") as HTMLElement)).focus(options);
  }

  async reveal(id: string): Promise<void> {
    const shown = this.show(id);
    this.scrollIntoView({ block: "nearest", behavior: reducedMotion() ? "auto" : "smooth" });
    await shown;
    this.#plotBox.focus({ preventScroll: true });
  }

  #fit(): void {
    const info = this.#loaded?.info;
    if (info) this.setWindow(0, info.length);
  }

  /** Zooms by `factor` (above 1 zooms out) about x, or the middle. */
  #zoomBy(factor: number, x: number | null): void {
    if (!this.#plot.layout) return;
    const { start, end } = this.#plot.view;
    const at = x === null ? (start + end) / 2 : this.#plot.tickAt(x);
    const span = Math.max(this.#minSpan(), (end - start) * factor);
    const f = (at - start) / (end - start);
    this.#plot.view = { start: at - f * span, end: at - f * span + span };
    this.#viewChanged();
  }

  #panBy(ticks: number): void {
    const { start, end } = this.#plot.view;
    this.#plot.view = { start: start + ticks, end: end + ticks };
    this.#viewChanged();
  }

  #viewChanged(): void {
    this.#clampView();
    this.#plot.invalidate();
    if (this.#windowEventQueued) return;
    this.#windowEventQueued = true;
    requestAnimationFrame(() => {
      this.#windowEventQueued = false;
      this.emit("tickwave-window", this.window);
    });
  }

  #drawn(): void {
    if (this.#readyFired || !this.#plot.settled) return;
    this.#readyFired = true;
    queueMicrotask(() => this.emit("tickwave-ready", { height: this.offsetHeight }));
  }

  /** Resolves after the next frame drawn with every row's data. */
  settled(): Promise<void> {
    return this.#plot.whenSettled();
  }

  /** The tick under a viewport x, or null before the view is drawn. */
  tickAt(clientX: number): number | null {
    if (!this.#plot.layout) return null;
    return this.#plot.tickAt(clientX - this.#plotBox.getBoundingClientRect().left);
  }

  // Pointer and keys ------------------------------------------------------------------------------

  #local(e: PointerEvent | WheelEvent | MouseEvent): { x: number; y: number } {
    const r = this.#plotBox.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  #listen(): void {
    const box = this.#plotBox;
    box.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || !this.#plot.layout) return;
      const p = this.#local(e);
      this.#pointers.set(e.pointerId, p);
      box.setPointerCapture(e.pointerId);
      if (this.#pointers.size === 2) {
        this.#drag = null;
        const [a, b] = [...this.#pointers.values()];
        this.#pinch = { distance: Math.abs(a.x - b.x) || 1, mid: (a.x + b.x) / 2, view: { ...this.#plot.view } };
        return;
      }
      const hit = this.#plot.hit(p.x, p.y);
      this.#drag = {
        id: e.pointerId,
        x: p.x,
        y: p.y,
        area: hit.area,
        row: hit.area === "row" ? hit.row : -1,
        view: { ...this.#plot.view },
        moved: false,
      };
    });
    box.addEventListener("pointermove", (e) => {
      const p = this.#local(e);
      if (this.#pointers.has(e.pointerId)) this.#pointers.set(e.pointerId, p);
      const pinch = this.#pinch;
      if (pinch && this.#pointers.size === 2) {
        const [a, b] = [...this.#pointers.values()];
        const distance = Math.abs(a.x - b.x) || 1;
        const span = ((pinch.view.end - pinch.view.start) * pinch.distance) / distance;
        const l = this.#plot.layout!;
        const f = (pinch.mid - l.plotLeft) / (l.plotRight - l.plotLeft);
        const anchor = pinch.view.start + f * (pinch.view.end - pinch.view.start);
        const mid = (a.x + b.x) / 2;
        const g = (mid - l.plotLeft) / (l.plotRight - l.plotLeft);
        this.#plot.view = { start: anchor - g * span, end: anchor - g * span + span };
        this.#viewChanged();
        return;
      }
      const d = this.#drag;
      if (d && d.id === e.pointerId) {
        if (!d.moved && Math.hypot(p.x - d.x, p.y - d.y) < DRAG_PX) return;
        d.moved = true;
        if (d.area === "axis") {
          const l = this.#plot.layout!;
          const inside = (x: number) => Math.max(l.plotLeft, Math.min(l.plotRight, x));
          this.#plot.selection = { start: this.#plot.tickAt(inside(d.x)), end: this.#plot.tickAt(inside(p.x)) };
          this.#plot.invalidate();
        } else {
          box.classList.add("panning");
          const per = (d.view.end - d.view.start) / (this.#plot.layout!.plotRight - this.#plot.layout!.plotLeft);
          this.#plot.view = { start: d.view.start - (p.x - d.x) * per, end: d.view.end - (p.x - d.x) * per };
          this.#viewChanged();
        }
        return;
      }
      this.#hover(p.x, p.y);
    });
    const end = (e: PointerEvent) => {
      this.#pointers.delete(e.pointerId);
      if (this.#pointers.size < 2) this.#pinch = null;
      const d = this.#drag;
      if (!d || d.id !== e.pointerId) return;
      this.#drag = null;
      box.classList.remove("panning");
      if (e.type === "pointercancel") return;
      const p = this.#local(e);
      if (d.moved) {
        if (d.area === "axis") this.#selected();
        return;
      }
      const hit = this.#plot.hit(p.x, p.y);
      const l = this.#plot.layout!;
      if (hit.area === "lane" && hit.note) void this.show(hit.note.id);
      else if (hit.area === "row" && p.x >= l.plotLeft && p.x <= l.plotRight) this.#placeCursor(p.x, hit.row);
    };
    box.addEventListener("pointerup", end);
    box.addEventListener("pointercancel", end);
    box.addEventListener("pointerleave", () => {
      if (this.#plot.hoverRow !== null || this.#plot.hoverNote !== null) {
        this.#plot.hoverRow = null;
        this.#plot.hoverNote = null;
        this.#plot.invalidate();
      }
    });
    box.addEventListener(
      "wheel",
      (e) => {
        if (!this.#plot.layout) return;
        const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? this.#plot.layout.width : 1;
        const dx = e.deltaX * unit;
        const dy = e.deltaY * unit;
        // Ctrl or Cmd zooms; a trackpad pinch arrives as a wheel with ctrlKey and small deltas.
        if (e.ctrlKey || e.metaKey) {
          e.preventDefault();
          this.#zoomBy(Math.exp(dy * 0.004), this.#local(e).x);
          return;
        }
        if (e.shiftKey || Math.abs(dx) > Math.abs(dy)) {
          e.preventDefault();
          this.#panBy((Math.abs(dx) > Math.abs(dy) ? dx : dy) * this.#plot.perPx);
          return;
        }
        // The plain wheel scrolls the page; say how to zoom instead.
        this.#hint();
      },
      { passive: false },
    );
    box.addEventListener("keydown", (e) => {
      const span = this.#plot.view.end - this.#plot.view.start;
      switch (e.key) {
        case "ArrowLeft":
          this.#panBy(-span * (e.shiftKey ? 0.5 : 0.125));
          break;
        case "ArrowRight":
          this.#panBy(span * (e.shiftKey ? 0.5 : 0.125));
          break;
        case "+":
        case "=":
          this.#zoomBy(1 / 1.6, null);
          break;
        case "-":
        case "_":
        case "−":
          this.#zoomBy(1.6, null);
          break;
        case "Home":
          this.#fit();
          break;
        case "Escape":
          this.#plot.cursors = { a: null, b: null };
          this.#plot.selection = null;
          this.#nextCursor = "a";
          this.#plot.invalidate();
          void this.#renderReadout();
          break;
        default:
          return;
      }
      e.preventDefault();
    });
  }

  #hintTimer = 0;

  #hint(): void {
    const hint = this.root.querySelector(".hint") as HTMLElement;
    const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
    const mac = /mac|iphone|ipad/i.test(nav.userAgentData?.platform || navigator.platform);
    hint.textContent = `${mac ? "Cmd" : "Ctrl"} + scroll to zoom`;
    hint.classList.add("shown");
    clearTimeout(this.#hintTimer);
    this.#hintTimer = window.setTimeout(() => hint.classList.remove("shown"), 1200);
  }

  #hover(x: number, y: number): void {
    const hit = this.#plot.hit(x, y);
    const row = hit.area === "row" ? hit.row : null;
    const note = hit.area === "lane" ? (hit.note?.id ?? null) : null;
    this.#plotBox.style.cursor = note ? "pointer" : hit.area === "axis" ? "col-resize" : "";
    if (row !== this.#plot.hoverRow || note !== this.#plot.hoverNote) {
      this.#plot.hoverRow = row;
      this.#plot.hoverNote = note;
      this.#plot.invalidate();
    }
  }

  /** A cursor snaps to the nearest edge of the clicked row within a few pixels, else to a tick. */
  #placeCursor(x: number, row: number): void {
    let tick = Math.round(this.#plot.tickAt(x));
    const tile = this.#plot.tracks[row]?.tile;
    if (tile?.kind === "edges") {
      const t = tile.data.ticks;
      const i = lowerBound(t, this.#plot.tickAt(x));
      let best = -1;
      for (const j of [i - 1, i]) {
        if (j >= 0 && j < t.length && (best < 0 || Math.abs(t[j] - this.#plot.tickAt(x)) < Math.abs(t[best] - this.#plot.tickAt(x)))) best = j;
      }
      if (best >= 0 && Math.abs(this.#plot.xAt(t[best]) - x) <= SNAP_PX) tick = t[best];
    }
    if (this.#nextCursor === "a") {
      this.#plot.cursors = { a: tick, b: null };
      this.#nextCursor = "b";
    } else {
      this.#plot.cursors = { a: this.#plot.cursors.a, b: tick };
      this.#nextCursor = "a";
    }
    this.#plot.invalidate();
    void this.#renderReadout();
  }

  #selected(): void {
    const s = this.#plot.selection;
    if (!s) return;
    const start = Math.round(Math.min(s.start, s.end));
    const end = Math.round(Math.max(s.start, s.end));
    this.#plot.selection = end > start ? { start, end } : null;
    this.#plot.invalidate();
    void this.#renderReadout(true);
  }

  // Readouts --------------------------------------------------------------------------------------

  async #renderReadout(announce = false): Promise<void> {
    const seq = ++this.#readoutSeq;
    const l = this.#loaded;
    if (!l) return;
    const tb = l.info.timebase;
    const lines: HTMLElement[] = [];
    const line = (...parts: (string | Node)[]) => {
      const div = document.createElement("div");
      div.className = "line";
      div.append(...parts);
      lines.push(div);
      return div;
    };
    const fig = (text: string) => Object.assign(document.createElement("span"), { className: "figures", textContent: text });
    const item = (name: string, value: string) => {
      const s = document.createElement("span");
      s.append(`${name} `, fig(value));
      return s;
    };
    const { a, b } = this.#plot.cursors;
    if (a !== null) {
      const parts = [item("A", formatInterval(a, tb))];
      if (b !== null) {
        parts.push(item("B", formatInterval(b, tb)));
        const dt = b - a;
        const rate = formatRate(1, Math.abs(dt), tb);
        parts.push(item("Δt", formatInterval(dt, tb) + (rate ? ` (${rate})` : "")));
      }
      line(...parts);
      // Each analog channel's value at the cursors, to the digit of one code step.
      for (const t of this.#plot.tracks) {
        const c = t.channel;
        if (c.kind !== "analog") continue;
        const at = async (tick: number) => {
          const i = sampleAtOrBefore(c, tick);
          if (i === null) return null;
          const from = sampleTick(c.first, c.period, i);
          const s = await l.source.samples(t.index, from, from + 1);
          return s.values.length > 0 ? s.values[0] : null;
        };
        let values: (number | null)[];
        try {
          values = await Promise.all([at(a), b !== null ? at(b) : null]);
        } catch (e) {
          if (!isAbort(e)) this.fail(e);
          return;
        }
        if (seq !== this.#readoutSeq) return;
        const parts: (string | Node)[] = [c.name];
        // A recording stores NaN samples as the source had them; no figure says that better than "–".
        const figure = (v: number | null) => (v === null || !Number.isFinite(v) ? "–" : formatValue(v, c.resolution, c.unit));
        parts.push(item("A", figure(values[0])));
        if (b !== null) {
          parts.push(item("B", figure(values[1])));
          if (values[0] !== null && values[1] !== null) parts.push(item("Δ", figure(values[1] - values[0])));
        }
        line(...parts);
      }
    }
    const sel = this.#plot.selection;
    let detail: Record<string, unknown> | null = null;
    if (sel) {
      line(item("Selection", `${formatInterval(sel.start, tb)} to ${formatInterval(sel.end, tb)}`), item("Δt", formatInterval(sel.end - sel.start, tb)));
      const counts = line();
      const periods = line();
      const digital = this.#plot.tracks.filter((t) => t.channel.kind === "digital");
      counts.append("…");
      this.#setReadout(lines);
      try {
        const results = await Promise.all(digital.map((t) => l.source.edges(t.index, sel.start, sel.end)));
        if (seq !== this.#readoutSeq) return;
        counts.textContent = "";
        const edgeCounts: Record<string, number> = {};
        digital.forEach((t, i) => {
          const n = countEdges(results[i].ticks, sel.start, sel.end);
          edgeCounts[t.channel.name] = n;
          counts.append(item(t.channel.name, `${formatCount(n)} edge${n === 1 ? "" : "s"}`));
        });
        const chosen = Math.max(0, digital.indexOf(this.#plot.tracks[this.#periodChannel]));
        const select = document.createElement("select");
        select.setAttribute("aria-label", "Channel for periods");
        for (const t of digital) select.append(new Option(t.channel.name, String(t.index)));
        select.selectedIndex = chosen;
        select.addEventListener("change", () => {
          this.#periodChannel = this.#plot.tracks.findIndex((t) => t.index === Number(select.value));
          void this.#renderReadout();
        });
        const label = document.createElement("label");
        label.append("Periods of", select);
        periods.append(label);
        const r = results[chosen];
        const stats = r ? periodStats(r.level, r.ticks) : null;
        let periodDetail: Record<string, unknown> | null = null;
        if (stats) {
          const rate = formatClockRate(stats, tb);
          const period = formatClockPeriod(stats, tb);
          periods.append(
            item("count", formatCount(stats.count)),
            item("min", formatInterval(stats.min, tb)),
            item("median", formatInterval(stats.median, tb)),
            item("max", formatInterval(stats.max, tb)),
            item("clock", rate),
            clockNote(stats),
          );
          periodDetail = { channel: digital[chosen].channel.name, ...stats, rate, period };
        } else periods.append("fewer than two rising edges");
        detail = { start: sel.start, end: sel.end, duration: formatInterval(sel.end - sel.start, tb), edges: edgeCounts, periods: periodDetail };
      } catch (e) {
        if (!isAbort(e)) this.fail(e);
        return;
      }
    }
    if (seq !== this.#readoutSeq) return;
    this.#setReadout(lines);
    if (announce && detail) this.emit("tickwave-select", detail);
  }

  #setReadout(lines: HTMLElement[]): void {
    this.#readout.replaceChildren(...lines);
  }

  // Stepping through one kind ----------------------------------------------------------------------

  #ofKind(): Annotation[] {
    const l = this.#loaded;
    const kind = this.#kind.value;
    if (!l || !kind) return [];
    return ofKind(l.annotations, kind);
  }

  #renderKinds(): void {
    const l = this.#loaded;
    if (!l) return;
    // In sidecar order: the first annotation's kind is the default.
    const kinds = [...new Set(l.annotations.list.map((a) => a.kind).filter((k): k is string => k !== null))];
    this.#steps.hidden = kinds.length === 0;
    const current = this.#kind.value;
    if (kinds.join("\n") !== [...this.#kind.options].map((o) => o.value).join("\n")) {
      this.#kind.replaceChildren(...kinds.map((k) => new Option(k, k)));
      if (kinds.includes(current)) this.#kind.value = current;
    }
    this.#renderSteps();
  }

  #renderSteps(): void {
    const list = this.#ofKind();
    const count = this.#steps.querySelector(".count")!;
    count.textContent = `${this.#stepIndex >= 0 ? formatCount(this.#stepIndex + 1) : "\u2013"} of ${formatCount(list.length)}`;
    (this.#steps.querySelector(".prev") as HTMLButtonElement).disabled = list.length === 0 || this.#stepIndex === 0;
    (this.#steps.querySelector(".next") as HTMLButtonElement).disabled = list.length === 0 || this.#stepIndex === list.length - 1;
  }

  #step(by: number): void {
    const list = this.#ofKind();
    if (list.length === 0) return;
    let i = this.#stepIndex;
    if (i < 0) {
      // From no position, step to the first one after (or the last before) the view's middle.
      const mid = (this.#plot.view.start + this.#plot.view.end) / 2;
      const after = list.findIndex((a) => a.start >= mid);
      i = by > 0 ? (after < 0 ? list.length - 1 : after) : (after < 0 ? list.length : after) - 1;
    } else i += by;
    i = Math.max(0, Math.min(list.length - 1, i));
    void this.show(list[i].id);
  }
}

/** The summary's facts by part name: "524 ms", "2 channels: SWCLK, SWDIO", "4.7 KiB". */
export function summaryFacts(info: RecordingInfo): [part: string, text: string][] {
  const names = info.channels.map((c) => c.name);
  const facts: [string, string][] = [
    ["fact-length", formatBrief(info.length, info.timebase)],
    ["fact-channels", `${names.length} channel${names.length === 1 ? "" : "s"}: ${names.join(", ")}`],
  ];
  if (info.bytes !== undefined) facts.push(["fact-size", formatBytes(info.bytes)]);
  return facts;
}

function part(name: string, text: string): HTMLSpanElement {
  const span = document.createElement("span");
  span.setAttribute("part", name);
  span.textContent = text;
  return span;
}
