// <tickwave-view>: the full capture. Collapsed to one line until `open`; open, a time axis, one row
// a channel and an annotation lane, with zoom, pan, cursors, a selection and its readouts.

import { ofKind, type Annotation } from "../annotations.ts";
import { parseChannels, parseWindow, VIEW_ATTRIBUTES } from "../attributes.ts";
import { reducedMotion } from "../face.ts";
import { absolute, events as registryEvents, infoFor, peekInfo } from "../registry.ts";
import { clockNote, countEdges, formatBrief, formatBytes, formatClockPeriod, formatClockRate, formatCount, formatInterval, formatRate, formatValue, periodStats } from "../readouts.ts";
import { analogFromOverview, analogFromSamples, complete, completePulse, pulseFigures, pulseFromEdges, type Pulse } from "../probe.ts";
import { lowerBound } from "../search.ts";
import { channelIndices, Plot, type PulseMark } from "../render/plot.ts";
import { isAbort, named, sampleAtOrBefore, sampleTick, type DigitalChannel, type RecordingInfo } from "../source.ts";
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
.reading { position: absolute; z-index: 1; pointer-events: none; padding: 3px 7px; border-radius: 5px;
  background: var(--raised, Canvas); border: 1px solid var(--hair, GrayText); color: var(--ink, CanvasText);
  font-size: 12px; line-height: 1.4; white-space: nowrap; font-variant-numeric: tabular-nums; }
.reading[hidden] { display: none; }
.reading .more { color: var(--ink-2, GrayText); }
.row-handle { position: absolute; left: 0; height: 8px; margin-top: -4px; cursor: row-resize; touch-action: none; }
@media (pointer: coarse) { .row-handle { height: 24px; margin-top: -12px; } }
.row-handle::after { content: ""; position: absolute; left: 4px; right: 4px; top: 50%; height: 2px; margin-top: -1px;
  border-radius: 1px; background: var(--ink, CanvasText); opacity: 0; }
.row-handle:hover::after, .row-handle:focus-visible::after, .row-handle.dragging::after { opacity: 1; }
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
  <div class="plot" part="plot" tabindex="0" role="group" aria-roledescription="capture"><canvas aria-hidden="true"></canvas><div class="hint" part="hint" aria-hidden="true"></div><div class="reading" part="reading" aria-hidden="true" hidden></div></div>
  <div class="readout" part="readout" aria-live="polite"></div>
</div>
<div class="notice" part="notice" role="status" hidden></div>`;

const DRAG_PX = 4;
/** A pulse whose edges lie past the data drawn is measured once the pointer rests this long. */
const REST_MS = 150;
const SNAP_PX = 6;
/** The heights a reader can give a row, in CSS pixels. */
const ROW_LIMITS = { digital: [24, 400], analog: [40, 600] } as const;
/** A handle's arrow keys move it this far, or the second with Shift. */
const ROW_STEP = [4, 16] as const;
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
  /** The recording and sidecar the attributes named at the last update. */
  #askedKey = "";
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
  /** One a row, on its lower edge in the names column. */
  #handles: HTMLElement[] = [];
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
    this.#plot.onDraw = () => {
      this.#drawn();
      this.#probeIfMoved();
    };
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

  /** A canvas cannot fade: it takes the new face in the frame the face turns, not the next. */
  protected override faceTurned(): void {
    super.faceTurned();
    this.#plot.drawNow();
  }

  protected update(): void {
    const open = this.open;
    this.#summary.hidden = open;
    this.#viewer.hidden = !open;
    if (open && !this.#wasOpen) this.emit("tickwave-open", { src: this.getAttribute("src") });
    this.#wasOpen = open;
    const src = this.getAttribute("src");
    const annotations = this.getAttribute("annotations");
    const key = src ? `${src}\n${annotations ?? ""}` : "";
    // Another recording or sidecar starts without the last one's notice, collapsed or not. The
    // next Open loads it afresh, so a sidecar's problems come back with it.
    if (key !== this.#askedKey) {
      this.#askedKey = key;
      this.#loadKey = "";
      this.clearProblems();
    }
    if (!open) {
      this.#fetchHead();
      this.#renderSummary();
      return;
    }
    if (!src) return;
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
          // A load the view has moved on from says nothing about the one it shows now. A failed
          // one is not kept: the next update, an Open or a show(), tries again.
          if (this.#loadKey !== key) return null;
          this.#loadKey = "";
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
    if (!src || !this.#inView || this.open) return;
    infoFor(src).catch((e) => {
      if (this.#src?.href === src.href) this.fail(e);
    });
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
      `${l.info.title}. Arrow keys pan, plus and minus zoom, Home fits the capture; Enter or a click places a cursor, drag on the time axis to select.`,
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
    this.#plot.heights.clear();
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
    this.#placeHandles();
    this.#clampView();
    this.#plot.invalidate();
  }

  // Row heights ----------------------------------------------------------------------------------

  #placeHandles(): void {
    const l = this.#plot.layout;
    if (!l) return;
    while (this.#handles.length > l.rows.length) this.#handles.pop()!.remove();
    while (this.#handles.length < l.rows.length) {
      const h = this.#handle(this.#handles.length);
      this.#handles.push(h);
      this.#plotBox.append(h);
    }
    l.rows.forEach((r, i) => {
      const h = this.#handles[i];
      const [min, max] = ROW_LIMITS[r.track.channel.kind];
      h.style.top = `${r.top + r.height}px`;
      h.style.width = `${l.plotLeft}px`;
      h.setAttribute("aria-label", `Height of ${r.track.channel.name}`);
      h.setAttribute("aria-valuenow", String(Math.round(r.height)));
      h.setAttribute("aria-valuemin", String(min));
      h.setAttribute("aria-valuemax", String(max));
    });
  }

  /**
   * The handle on row `row`'s lower edge: a drag sets the row's height, as do the arrow keys; Home
   * and End give it its least and greatest, and Enter or a double click its default.
   */
  #handle(row: number): HTMLElement {
    const h = Object.assign(document.createElement("div"), { className: "row-handle", tabIndex: 0 });
    h.setAttribute("part", "row-handle");
    h.setAttribute("role", "separator");
    h.setAttribute("aria-orientation", "horizontal");
    let drag: { id: number; y: number; height: number } | null = null;
    // The plot's own handlers pan, measure and place cursors: none of that while on a handle.
    h.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      const r = this.#plot.layout?.rows[row];
      if (e.button !== 0 || !r) return;
      drag = { id: e.pointerId, y: e.clientY, height: r.height };
      h.setPointerCapture(e.pointerId);
      h.classList.add("dragging");
      this.#hideProbe();
    });
    h.addEventListener("pointermove", (e) => {
      e.stopPropagation();
      if (drag?.id === e.pointerId) this.#setHeight(row, drag.height + e.clientY - drag.y);
    });
    const end = (e: PointerEvent) => {
      e.stopPropagation();
      if (drag?.id !== e.pointerId) return;
      drag = null;
      h.classList.remove("dragging");
    };
    h.addEventListener("pointerup", end);
    h.addEventListener("pointercancel", end);
    h.addEventListener("dblclick", (e) => {
      e.stopPropagation();
      this.#setHeight(row, null);
    });
    h.addEventListener("keydown", (e) => {
      const r = this.#plot.layout?.rows[row];
      if (!r) return;
      const [min, max] = ROW_LIMITS[r.track.channel.kind];
      const step = ROW_STEP[e.shiftKey ? 1 : 0];
      let to: number | null;
      if (e.key === "ArrowUp") to = r.height - step;
      else if (e.key === "ArrowDown") to = r.height + step;
      else if (e.key === "Home") to = min;
      else if (e.key === "End") to = max;
      else if (e.key === "Enter") to = null;
      else return;
      e.preventDefault();
      e.stopPropagation();
      this.#setHeight(row, to);
    });
    return h;
  }

  /** Gives row `row` a height, within its kind's limits, or its default for null. */
  #setHeight(row: number, height: number | null): void {
    const t = this.#plot.tracks[row];
    if (!t) return;
    const [min, max] = ROW_LIMITS[t.channel.kind];
    if (height === null) this.#plot.heights.delete(t.index);
    else this.#plot.heights.set(t.index, Math.round(Math.max(min, Math.min(max, height))));
    this.#layout(true);
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
    this.#probe();
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
      if (e.pointerType === "mouse" || e.pointerType === "pen") this.#pointer = p;
      if (this.#pointers.has(e.pointerId)) this.#pointers.set(e.pointerId, p);
      const pinch = this.#pinch;
      if (pinch && this.#pointers.size === 2) {
        this.#hideProbe();
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
        this.#hideProbe();
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
      this.#probe();
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
        this.#probe();
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
      this.#pointer = null;
      this.#hideProbe();
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
        case "Enter":
          this.#placeCursorAt(Math.round((this.#plot.view.start + this.#plot.view.end) / 2));
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
    this.#placeCursorAt(tick);
  }

  /** Places cursor A, or B after it, at `tick`. */
  #placeCursorAt(tick: number): void {
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

  // The label beside the pointer ------------------------------------------------------------------

  /** The pointer over the plot (a mouse or pen, not a finger), in the plot's coordinates. */
  #pointer: { x: number; y: number } | null = null;
  #probeTimer = 0;
  #probeAbort: AbortController | null = null;
  /** What the label last measured from: a draw that changes none of it leaves the label be. */
  #probeFrom: { x: number; y: number; start: number; end: number; tile: unknown } | null = null;

  #probeBox(): HTMLElement {
    return this.root.querySelector(".reading") as HTMLElement;
  }

  #hideProbe(): void {
    clearTimeout(this.#probeTimer);
    this.#probeAbort?.abort();
    this.#probeAbort = null;
    this.#probeFrom = null;
    this.#probeBox().hidden = true;
    this.#setMark(null);
  }

  /** Measures again when a draw has brought new data or a new view under the pointer. */
  #probeIfMoved(): void {
    const p = this.#pointer;
    const f = this.#probeFrom;
    if (!p || !f) return;
    const row = this.#plot.hit(p.x, p.y);
    const tile = row.area === "row" ? this.#plot.tracks[row.row]?.tile : null;
    if (f.x !== p.x || f.y !== p.y || f.start !== this.#plot.view.start || f.end !== this.#plot.view.end || f.tile !== tile) this.#probe();
  }

  /**
   * The row under the pointer, measured at its pixel from the data the row has drawn: an analog
   * value, or the range the pixel draws; a digital pulse's level, width, period, frequency and duty.
   * A pulse whose edges lie past the data drawn is finished once the pointer rests.
   */
  #probe(): void {
    clearTimeout(this.#probeTimer);
    this.#probeAbort?.abort();
    this.#probeAbort = null;
    const box = this.#probeBox();
    const p = this.#pointer;
    const l = this.#plot.layout;
    const loaded = this.#loaded;
    const hit = p && l ? this.#plot.hit(p.x, p.y) : null;
    if (!p || !l || !loaded || !hit || hit.area !== "row" || this.#drag?.moved || this.#pinch || p.x < l.plotLeft || p.x >= l.plotRight) {
      this.#probeFrom = null;
      box.hidden = true;
      this.#setMark(null);
      return;
    }
    const track = this.#plot.tracks[hit.row];
    const tile = track?.tile ?? null;
    this.#probeFrom = { x: p.x, y: p.y, start: this.#plot.view.start, end: this.#plot.view.end, tile };
    const col = Math.floor(p.x - l.plotLeft) + l.plotLeft;
    const t0 = this.#plot.tickAt(col);
    const t1 = this.#plot.tickAt(col + 1);
    const tick = Math.floor(this.#plot.tickAt(p.x));
    const c = track.channel;
    if (!tile || tick < tile.start || tick >= tile.end || tick < 0 || tick > c.end) {
      box.hidden = true;
      this.#setMark(null);
      return;
    }
    let lines: string[] | null = null;
    if (c.kind === "analog") {
      const at = tile.kind === "samples" ? analogFromSamples(c, tile.data, t0, t1, tick) : tile.kind === "analog-overview" ? analogFromOverview(tile.data, t0, t1) : null;
      const v = (x: number) => formatValue(x, c.resolution, c.unit);
      if (at?.kind === "value") lines = [v(at.value)];
      else if (at?.kind === "range") lines = [`${v(at.min)} to ${v(at.max)}`];
    } else {
      let pulse: Pulse | null = null;
      if (tile.kind === "edges") {
        const t = tile.data.ticks;
        // Two edges in the pixel: no one pulse is under the pointer.
        if (lowerBound(t, Math.ceil(t1)) - lowerBound(t, Math.ceil(t0)) > 1) lines = ["zoom in to measure"];
        else pulse = pulseFromEdges(tile.data, tile.start, tile.end, c.end, tick);
      } else if (tile.kind === "digital-overview") {
        const o = tile.data;
        const k0 = Math.max(0, Math.floor((t0 - o.start) / o.bucket));
        const k1 = Math.min(o.changes.length, Math.ceil((t1 - o.start) / o.bucket));
        let changes = false;
        for (let k = k0; k < k1; k++) changes ||= o.changes[k] === 1;
        if (changes) lines = ["zoom in to measure"];
        else if (k0 < o.level.length) pulse = { level: o.level[k0] ? 1 : 0, start: undefined, end: undefined, next: undefined };
      }
      if (pulse && complete(pulse)) {
        this.#showPulse(pulse, c, loaded.info, p, hit.row);
        return;
      }
      // A row whose last read failed waits before it reads again, and so does the label: the level only.
      else if (pulse && track.failed) lines = [pulse.level ? "high" : "low"];
      else if (pulse) {
        const asked = pulse;
        lines = [asked.level ? "high" : "low", "measuring…"];
        const abort = new AbortController();
        this.#probeAbort = abort;
        this.#probeTimer = window.setTimeout(() => {
          completePulse(loaded.source, track.index, asked, tick, c.end, abort.signal).then(
            (done) => {
              if (!abort.signal.aborted) this.#showPulse(done, c, loaded.info, p, hit.row);
            },
            // A measurement that fails says nothing: the row's own read reports the trouble.
            () => {
              if (!abort.signal.aborted) box.hidden = true;
            },
          );
        }, REST_MS);
      }
    }
    this.#setMark(null);
    if (!lines) box.hidden = true;
    else this.#showProbe(lines, p, l.rows[hit.row]);
  }

  #setMark(m: PulseMark | null): void {
    const o = this.#plot.pulseMark;
    const same = o === m || (o !== null && m !== null && (Object.keys(m) as (keyof PulseMark)[]).every((k) => o[k] === m[k]));
    if (same) return;
    this.#plot.pulseMark = m;
    this.#plot.invalidate();
  }

  /**
   * A complete pulse: its spans on the trace, with the width and the period on them where the row
   * has room, and the label beside the edge that closes the period with the figures left over.
   */
  #showPulse(pulse: Pulse, c: DigitalChannel, info: RecordingInfo, p: { x: number; y: number }, row: number): void {
    const f = pulseFigures(pulse, info.timebase, c.end);
    const { start, end } = pulse;
    const mark: PulseMark | null =
      typeof start === "number" && typeof end === "number"
        ? { row, level: pulse.level, start, end, next: pulse.next ?? null, width: f.width, period: f.period }
        : null;
    const on = mark ? this.#plot.markFits(mark) : { width: false, period: false };
    this.#setMark(mark);
    const lines: string[] = [];
    if (!on.width) lines.push(`${f.level} ${f.width}`);
    if (f.period) {
      if (!on.period) lines.push(`period ${f.period}`);
      lines.push(`duty ${f.duty}`, f.frequency ?? "–");
    }
    if (f.inverseWidth) lines.push(`1/width ${f.inverseWidth}`);
    const beside = mark && { x0: this.#plot.xAt(mark.start), x1: this.#plot.xAt(mark.next ?? mark.end) };
    this.#showProbe(lines, p, this.#plot.layout!.rows[row], beside);
  }

  /**
   * The label, level with its row: beside the edges of the pulse it measures, right of the closing
   * one or left of the first, else beside the pointer, on its left near the plot's right edge.
   */
  #showProbe(lines: string[], p: { x: number; y: number }, row: { top: number; height: number }, beside: { x0: number; x1: number } | null = null): void {
    const box = this.#probeBox();
    box.replaceChildren(
      ...lines.map((text, i) => Object.assign(document.createElement("div"), { textContent: text, className: i > 0 ? "more" : "" })),
    );
    box.hidden = false;
    const l = this.#plot.layout!;
    const w = box.offsetWidth;
    const h = box.offsetHeight;
    const fits = (x: number) => x >= l.plotLeft && x + w <= l.plotRight;
    const after = beside ? beside.x1 + 10 : NaN;
    const before = beside ? beside.x0 - 10 - w : NaN;
    const right = p.x + 14;
    const x = fits(after) ? after : fits(before) ? before : right + w <= l.plotRight ? right : Math.max(l.plotLeft, p.x - 14 - w);
    const y = Math.max(0, Math.min(this.#plotBox.clientHeight - h, row.top + row.height / 2 - h / 2));
    box.style.left = `${x}px`;
    box.style.top = `${y}px`;
  }

  // Readouts --------------------------------------------------------------------------------------

  async #renderReadout(announce = false): Promise<void> {
    const seq = ++this.#readoutSeq;
    const l = this.#loaded;
    if (!l) return;
    // A readout another has replaced says nothing of its failures: they may be the last recording's.
    const failed = (e: unknown) => {
      if (seq === this.#readoutSeq && !isAbort(e)) this.fail(e);
    };
    const tb = l.info.timebase;
    const lines: HTMLElement[] = [];
    const line = (...parts: (string | Node)[]) => {
      const div = document.createElement("div");
      div.className = "line";
      // A space between items, which the flex gap draws: without it, the live region reads them run together.
      div.append(...parts.flatMap((part, i) => (i > 0 ? [" ", part] : [part])));
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
          failed(e);
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
      // Each digital channel's pulse at cursor A, as the label beside the pointer gives it.
      for (const t of this.#plot.tracks) {
        const c = t.channel;
        if (c.kind !== "digital" || a < 0 || a > c.end) continue;
        let pulse: Pulse;
        try {
          const e = await l.source.edges(t.index, a, a + 1);
          pulse = await completePulse(l.source, t.index, pulseFromEdges(e, a, a + 1, c.end, a), a, c.end);
        } catch (e) {
          failed(e);
          return;
        }
        if (seq !== this.#readoutSeq) return;
        const f = pulseFigures(pulse, tb, c.end);
        const parts: (string | Node)[] = [c.name, item(f.level, f.width)];
        if (f.inverseWidth) parts.push(item("1/width", f.inverseWidth));
        if (f.period) parts.push(item("period", f.period), fig(f.frequency ?? "–"), item("duty", f.duty ?? "–"));
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
        failed(e);
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
