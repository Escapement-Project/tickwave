// <tickwave-inset>: one annotation's span, magnified, with its label, the annotations inside it as
// notes, and an optional `measure` readout. It sweeps in left to right once per appearance. A reader
// can point at a note or a pulse to read it, zoom in and move within the span, step through the notes
// by keys, and open the recording at a note, over the span zoomed to: in a view on the page (`for`) or
// on another page (`href`).

import { covers, type Annotation, type MeasureKind } from "../annotations.ts";
import { INSET_ATTRIBUTES, parseChannels, parsePad, parseScale, parseSweep } from "../attributes.ts";
import { escapeHtml as esc } from "../escape.ts";
import { reducedMotion } from "../face.ts";
import { Prober, ZOOM_IN, type Probed } from "../prober.ts";
import { formatInterval, measureReading } from "../readouts.ts";
import { channelIndices, Plot, ROLE_WEIGHT } from "../render/plot.ts";
import { isAbort, named, type RecordingInfo } from "../source.ts";
import { secondsValue, toTicks } from "../time.ts";
import { DRAG_PX, TickwaveElement, loadRecording, zoomKey, type Loaded } from "./base.ts";
import type { TickwaveView } from "./view.ts";

const CSS = `
:host { max-width: 100%; }
figure { margin: 0; }
figcaption { margin: 0 4px 2px; font-size: 12px; font-weight: 600; line-height: 1.35; }
.text { margin: 0 4px 4px; font-size: 12px; line-height: 1.4; color: var(--ink-2, GrayText); }
.frame { position: relative; }
canvas { display: block; }
/* Over the plot: what the pointer and the keys reach. A link where the inset opens another page. */
/* Two fingers zoom the trace, not the page. */
.hit { position: absolute; inset: 0; display: block; border-radius: 4px; touch-action: pan-y; -webkit-tap-highlight-color: transparent; }
.hit:focus { outline: none; }
.hit:focus-visible { outline: var(--focus-width, 2px) solid var(--ink, CanvasText); outline-offset: 2px; }
.hit.opens { cursor: pointer; }
.hit.panning { cursor: grabbing; }
.foot { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: baseline; gap: 2px 14px;
  margin: 2px 4px 0; font-size: 12px; color: var(--ink-2, GrayText); }
.foot:empty { display: none; }
/* The first line: the measure, or while the reader points at a part, its reading, whole items only.
   It is held once the annotation asks for a measure or the inset can be pointed at, and the key has
   a line of its own: the inset's height is set when its sidecar arrives, and neither its data nor
   the pointer moves anything. */
.line { flex: 1 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; }
.line::before { content: "\\200b"; }
.key:empty, .line.reads .measure, .line:not(.reads) .reading { display: none; }
.measure, .reading { color: var(--ink, CanvasText); }
.measure .what, .reading .what { font-family: var(--font-text, system-ui, sans-serif); color: var(--ink-2, GrayText); }
.reading > span + span { margin-left: 10px; }
/* A level is as wide as "high" either way, so what follows it holds still as the pointer crosses
   edges. */
.level { display: inline-grid; }
.level > span, .level::after { grid-area: 1 / 1; }
.level::after { content: "high"; visibility: hidden; }
.said { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
.key { display: flex; gap: 12px; flex-basis: 100%; justify-content: flex-end; }
.key i { display: inline-block; width: 14px; height: 2px; vertical-align: middle; margin-right: 5px; }
.key i.heavy { height: 3px; }
`;

const HTML = `<figure part="inset"><figcaption part="label"></figcaption><p class="text" part="text" hidden></p>
<div class="frame" part="frame"><canvas part="canvas" aria-hidden="true"></canvas><a class="hit" part="plot" hidden></a></div>
<div class="foot" part="foot"><span class="line" hidden><span class="measure figures" part="measure" hidden></span><span class="reading" part="reading" aria-hidden="true"></span></span><span class="key" part="key"></span></div>
<span class="said" aria-live="polite"></span>
<div class="notice" part="notice" role="status" hidden></div></figure>`;

/** The landing page's sweep: steady across the trace. */
const SWEEP_MS = 420;
/** Reopened this soon after closing, an inset shows already drawn. */
const REAPPEAR_MS = 800;

/** A reading's item: its pieces, each plain, a name (`what`), a level or a figure. */
type Item = readonly (readonly ["" | "what" | "level" | "figures", string])[];

export class TickwaveInset extends TickwaveElement {
  static observedAttributes = INSET_ATTRIBUTES;
  readonly #plot: Plot;
  readonly #caption: HTMLElement;
  readonly #measure: HTMLElement;
  readonly #key: HTMLElement;
  readonly #frame: HTMLElement;
  readonly #hit: HTMLAnchorElement;
  readonly #line: HTMLElement;
  readonly #reading: HTMLElement;
  readonly #said: HTMLElement;
  readonly #prober: Prober;
  /** Whether a reader can point at it: a chart's popover, for one, lets the pointer through. */
  #readable = false;
  /** The note lit by the pointer or the keys. */
  #lit: string | null = null;
  /** The note lit as the current touch began, and whether one is under way: a tap on it opens. */
  #litAtTouch: string | null = null;
  #touch = false;
  /** Where the pointer last read, in the plot's coordinates: it reads again as the span moves. */
  #pointer: { x: number; y: number } | null = null;
  /** The last pointer's kind: the line says how to zoom with it. */
  #pointerType = "mouse";
  /** The span the reader has zoomed to, inside the inset's own; null shows all of it. */
  #zoomed: { start: number; end: number } | null = null;
  /** The fingers on the plot, at their x; two pinch. */
  readonly #fingers = new Map<number, number>();
  /** A pinch under way: the fingers' spread and middle as it began, and the span then. */
  #pinch: { distance: number; mid: number; view: { start: number; end: number } } | null = null;
  /** A press on the zoomed-in plot, which moves the span once it has moved DRAG_PX, and the span as
   * it began. */
  #drag: { id: number; x: number; y: number; view: { start: number; end: number }; moved: boolean } | null = null;
  /** The last press ended a drag: its click opens nothing. */
  #dragged = false;
  #loaded: Loaded | null = null;
  #loadKey = "";
  #annotation: Annotation | null = null;
  #recordingKey = "";
  #layoutKey = "";
  #width = 0;
  #visible = false;
  #inViewport = false;
  #hiddenAt = -Infinity;
  #sweepWanted = true;
  #sweepFrame = 0;
  #ready = "";
  /** The sweep's wait for the plot to settle, by load and annotation: one at a time. */
  #awaiting = "";
  /** The missing annotation the notice names, until another is asked for. */
  #missing: string | null = null;
  #measureSeq = 0;
  #resize: ResizeObserver;
  #intersect: IntersectionObserver;

  constructor() {
    super(CSS, HTML);
    this.#caption = this.root.querySelector("figcaption")!;
    this.#measure = this.root.querySelector(".measure")!;
    this.#key = this.root.querySelector(".key")!;
    this.#frame = this.root.querySelector(".frame")!;
    this.#plot = new Plot(this.root.querySelector("canvas")!, {
      axis: false,
      lanes: 1,
      rowHeight: 26,
      analogHeight: 56,
      noteRules: true,
      labelPx: 11,
    });
    this.#plot.onError = (e) => this.fail(e);
    this.#plot.onRecovered = (m) => this.forgetProblem(m);
    this.#plot.onStall = (ms) => this.waiting(ms);
    this.#hit = this.root.querySelector(".hit")!;
    this.#line = this.root.querySelector(".line")!;
    this.#reading = this.root.querySelector(".reading")!;
    this.#said = this.root.querySelector(".said")!;
    this.#prober = new Prober(this.#plot, () => this.#loaded, (r) => this.#probed(r));
    this.#plot.onDraw = () => this.#prober.probeIfMoved();
    const hit = this.#hit;
    // A link's own drag would carry its address away instead.
    hit.draggable = false;
    hit.addEventListener("pointerdown", (e) => {
      this.#touch = e.pointerType === "touch";
      this.#dragged = false;
      if (this.#touch && this.#fingerDown(e)) return;
      this.#litAtTouch = this.#lit;
      this.#point(e);
      if (e.button === 0 && this.#zoomed) {
        const p = this.#local(e);
        this.#drag = { id: e.pointerId, x: p.x, y: p.y, view: { ...this.#plot.view }, moved: false };
      }
    });
    hit.addEventListener("pointermove", (e) => {
      if (this.#fingers.has(e.pointerId)) this.#fingers.set(e.pointerId, this.#local(e).x);
      if (this.#pinch) this.#pinchMoved();
      else if (!this.#dragMoved(e)) this.#point(e);
    });
    const lifted = (e: PointerEvent) => {
      this.#fingers.delete(e.pointerId);
      if (this.#fingers.size < 2) this.#pinch = null;
      const d = this.#drag;
      if (d?.id !== e.pointerId) return;
      this.#drag = null;
      this.#dragged = d.moved && e.type === "pointerup";
      hit.classList.remove("panning");
    };
    hit.addEventListener("pointerup", lifted);
    hit.addEventListener("pointercancel", lifted);
    // A finger's pointer leaves as it lifts: what it lit stays until the next tap.
    hit.addEventListener("pointerleave", (e) => {
      if (e.pointerType !== "touch") this.clear();
    });
    hit.addEventListener("click", (e) => this.#clicked(e));
    hit.addEventListener("keydown", (e) => this.#keyed(e));
    hit.addEventListener("wheel", (e) => this.#wheeled(e), { passive: false });
    this.#resize = new ResizeObserver(() => this.#sized());
    this.#intersect = new IntersectionObserver((seen) => {
      this.#inViewport = seen.some((s) => s.isIntersecting);
      this.#maybeSweep();
    });
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.#resize.observe(this);
    this.#intersect.observe(this);
    addEventListener("beforeprint", this.#print);
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.#resize.disconnect();
    this.#intersect.disconnect();
    removeEventListener("beforeprint", this.#print);
    this.#becameHidden();
  }

  #becameHidden(): void {
    if (this.#visible) this.#hiddenAt = performance.now();
    this.#visible = false;
    cancelAnimationFrame(this.#sweepFrame);
    this.#sweepFrame = 0;
  }

  #sized(): void {
    const shown = this.offsetWidth > 0 && this.offsetHeight > 0;
    if (!shown) {
      this.#becameHidden();
      return;
    }
    let fresh = false;
    if (!this.#visible) {
      this.#visible = true;
      // A tap can close and reopen an inset as the finger lifts; that is not a new appearance.
      this.#sweepWanted = performance.now() - this.#hiddenAt >= REAPPEAR_MS;
      if (this.#sweepWanted) this.#plot.reveal = this.#plot.layout?.plotLeft ?? 0;
      else this.#plot.reveal = null;
      // A new appearance shows the whole span.
      fresh = this.#sweepWanted && this.#zoomed !== null;
      if (fresh) this.#zoomed = null;
    }
    const width = this.#frame.clientWidth;
    if (width > 0 && (width !== this.#width || fresh)) {
      this.#width = width;
      this.#layout();
    }
    this.#maybeSweep();
  }

  /** The canvas takes the new face in the frame the page's colours change, not the next. */
  protected override faceTurned(): void {
    super.faceTurned();
    this.#plot.drawNow();
  }

  protected paletteChanged(): void {
    this.#plot.palette = this.palette;
    this.#layoutKey = "";
    this.#layout();
    this.#renderKey();
  }

  protected update(): void {
    const src = this.getAttribute("src");
    if (!src) return;
    const annotations = this.getAttribute("annotations");
    const key = `${src}\n${annotations ?? ""}`;
    if (key !== this.#loadKey) {
      this.#loadKey = key;
      this.#loaded = null;
      this.#missing = null;
      this.clearProblems();
      loadRecording(src, annotations).then(
        (l) => {
          if (this.#loadKey !== key) return;
          this.#loaded = l;
          for (const e of l.annotations.errors) this.fail(named("TickwaveSidecarError", e));
          this.#show();
        },
        (e) => {
          // A load the inset has moved on from says nothing about the one it shows now. A failed
          // one is not kept: the next update tries again.
          if (this.#loadKey !== key) return;
          this.#loadKey = "";
          this.fail(e);
        },
      );
      return;
    }
    this.#show();
  }

  /** Whether the sweep is playing. */
  get sweeping(): boolean {
    return this.#sweepFrame !== 0;
  }

  /** Resolves after the next frame drawn with every row's data. */
  settled(): Promise<void> {
    return this.#plot.whenSettled();
  }

  /** A printed page shows every inset whole, swept or not. */
  #print = () => {
    cancelAnimationFrame(this.#sweepFrame);
    this.#sweepFrame = 0;
    this.#sweepWanted = false;
    this.#plot.reveal = null;
    this.#plot.drawNow();
  };

  /** The span shown, `{start, end}` in ticks, or null before the inset has laid out. */
  get window(): { start: number; end: number } | null {
    return this.#plot.layout && this.#annotation ? { ...this.#plot.view } : null;
  }

  /** The span the reader has zoomed to, `{start, end}` in ticks, or null while the whole span shows.
   * The inset opens its recording there. */
  get zoomed(): { start: number; end: number } | null {
    return this.#zoomed && { ...this.#zoomed };
  }

  /** The annotation shown, once the recording is here. */
  get annotation(): Annotation | null {
    return this.#annotation;
  }

  #show(): void {
    const l = this.#loaded;
    if (!l) return;
    const id = this.getAttribute("annotation");
    const a = id ? (l.annotations.byId.get(id) ?? null) : null;
    // One inset can show annotation after annotation, a chart's popover for one: a missing one
    // stays in the notice only while it is the one asked for.
    const absent = id && !a ? `no annotation ${id}` : null;
    if (this.#missing !== null && this.#missing !== absent) this.forgetProblem(this.#missing);
    this.#missing = absent;
    if (absent) this.fail(new Error(absent));
    if (a?.id !== this.#annotation?.id) {
      this.#sweepWanted = true;
      this.#plot.reveal = this.#plot.layout?.plotLeft ?? 0;
      this.#zoomed = null;
    }
    this.#annotation = a;
    this.#caption.textContent = a ? (a.label ?? a.id) : "";
    const text = this.root.querySelector(".text") as HTMLElement;
    text.textContent = a?.text ?? "";
    text.hidden = !a?.text;
    const { indices, missing } = channelIndices(l.info, parseChannels(this.getAttribute("channels")));
    for (const m of missing) this.fail(new Error(`no channel ${m}`));
    const recordingKey = `${l.src.href}\n${indices.join(" ")}`;
    if (recordingKey !== this.#recordingKey) {
      this.#recordingKey = recordingKey;
      this.#zoomed = null;
      this.#plot.setRecording(l.source, l.info, indices);
      this.#layoutKey = "";
    }
    this.#plot.annotations = l.annotations;
    this.#plot.focus = a?.id ?? null;
    this.#readable = getComputedStyle(this).pointerEvents !== "none";
    if (this.#lit !== null && !l.annotations.byId.has(this.#lit)) this.clear();
    this.#layout();
    this.#renderMeasure();
    this.#renderKey();
    this.#renderHit();
  }

  #layout(): void {
    const l = this.#loaded;
    if (!l || !this.palette || this.#width <= 0) return;
    // The lane shows only the annotations inside the span, and its height depends on whether
    // there are any, so the filter is set before the layout.
    const inside = this.#inside(l);
    this.#plot.laneFilter = inside;
    const key = `${this.#recordingKey}\t${this.#width}\t${inside.size > 0}`;
    if (key !== this.#layoutKey) {
      this.#layoutKey = key;
      this.#plot.resize(this.#width, null);
    }
    const w = this.#window(l.info);
    if (w) {
      this.#zoomed = this.#zoomed && this.#inSpan(this.#zoomed);
      this.#plot.view = this.#zoomed ?? w;
    }
    if (this.#sweepWanted && this.#plot.reveal !== null) this.#plot.reveal = this.#plot.layout!.plotLeft;
    this.#plot.invalidate();
    this.#renderHit();
    this.#maybeSweep();
  }

  /** Annotations wholly inside the span, other than the span's own. */
  #inside(l: Loaded): Set<string> {
    const a = this.#annotation;
    const ids = new Set<string>();
    if (!a || a.end === null) return ids;
    const names = new Set(this.#plot.tracks.map((t) => t.channel.name));
    for (const b of l.annotations.list) {
      if (b.id === a.id || b.id === a.ref || b.end === null) continue;
      if (b.start >= a.start && b.end <= a.end && (b.channels === null || b.channels.some((c) => names.has(c)))) ids.add(b.id);
    }
    return ids;
  }

  #window(info: RecordingInfo): { start: number; end: number } | null {
    const a = this.#annotation;
    const layout = this.#plot.layout;
    if (!a || !layout) return null;
    const span = (a.end ?? a.start) - a.start;
    const pad = parsePad(this.getAttribute("pad"));
    let padTicks = "fraction" in pad ? pad.fraction * span : toTicks(pad.time, info.timebase);
    // A point has no span to take a fraction of.
    if (span === 0 && "fraction" in pad) padTicks = Math.max(1000, info.length / 2000);
    const scale = parseScale(this.getAttribute("scale"));
    const start = a.start - padTicks;
    if (scale) {
      const perPx = (secondsValue(scale) * info.timebase.num) / info.timebase.den;
      return { start, end: start + (layout.plotRight - layout.plotLeft) * perPx };
    }
    return { start, end: Math.max(start + 1, (a.end ?? a.start) + padTicks) };
  }

  // Zoom ------------------------------------------------------------------------------------------

  /** `v` moved and widened to lie within the inset's own span; null where it takes in all of it. */
  #inSpan(v: { start: number; end: number }): { start: number; end: number } | null {
    const w = this.#loaded ? this.#window(this.#loaded.info) : null;
    if (!w) return null;
    const span = Math.max(this.#plot.minSpan, v.end - v.start);
    if (span >= w.end - w.start) return null;
    const start = Math.min(Math.max(w.start, (v.start + v.end) / 2 - span / 2), w.end - span);
    return { start, end: start + span };
  }

  /** Shows `v`, within the inset's span, and reads again at the pointer. */
  #zoomTo(v: { start: number; end: number }): void {
    const w = this.#loaded ? this.#window(this.#loaded.info) : null;
    if (!w) return;
    this.#zoomed = this.#inSpan(v);
    this.#plot.view = this.#zoomed ?? w;
    this.#renderHit();
    // The notes take their places in the new span before the pointer finds one.
    this.#plot.drawNow();
    if (this.#pointer) this.#pointAt(this.#pointer);
  }

  /** Zooms by `factor` (above 1 zooms out), keeping tick `at` where it is. */
  #zoomBy(factor: number, at: number): void {
    const { start, end } = this.#plot.view;
    const span = Math.max(this.#plot.minSpan, (end - start) * factor);
    const f = (at - start) / (end - start);
    this.#zoomTo({ start: at - f * span, end: at - f * span + span });
  }

  #panBy(ticks: number): void {
    const { start, end } = this.#plot.view;
    this.#zoomTo({ start: start + ticks, end: end + ticks });
  }

  /** Ctrl or Cmd with the wheel, and a trackpad's pinch, zoom about the pointer. Zoomed in, a
   * sideways swipe or Shift with the wheel moves along the span. Otherwise the page scrolls. */
  #wheeled(e: WheelEvent): void {
    const l = this.#plot.layout;
    if (!l || !this.#loaded) return;
    const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? l.width : 1;
    const dx = e.deltaX * unit;
    const dy = e.deltaY * unit;
    const p = this.#local(e);
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      this.#pointer = p;
      this.#zoomBy(Math.exp(dy * 0.004), this.#plot.tickAt(p.x));
    } else if (this.#zoomed && (e.shiftKey || Math.abs(dx) > Math.abs(dy))) {
      e.preventDefault();
      this.#pointer = p;
      this.#panBy((Math.abs(dx) > Math.abs(dy) ? dx : dy) * this.#plot.perPx);
    }
  }

  /** Moves the span with a drag under way; true if `e` belongs to one past DRAG_PX. The tick under
   * the pointer as the drag began stays under it. */
  #dragMoved(e: PointerEvent): boolean {
    const d = this.#drag;
    const l = this.#plot.layout;
    if (!d || d.id !== e.pointerId || !l) return false;
    const p = this.#local(e);
    if (!d.moved && Math.hypot(p.x - d.x, p.y - d.y) < DRAG_PX) return false;
    if (!d.moved) {
      d.moved = true;
      this.#hit.setPointerCapture(e.pointerId);
      this.#hit.classList.add("panning");
    }
    const shift = ((p.x - d.x) * (d.view.end - d.view.start)) / (l.plotRight - l.plotLeft);
    this.#pointerType = e.pointerType;
    this.#pointer = p;
    this.#zoomTo({ start: d.view.start - shift, end: d.view.end - shift });
    return true;
  }

  /** Takes a finger down; true if it begins a pinch, or comes after one, rather than pointing. */
  #fingerDown(e: PointerEvent): boolean {
    if (this.#fingers.size >= 2) return true;
    this.#fingers.set(e.pointerId, this.#local(e).x);
    if (this.#fingers.size < 2) return false;
    const [a, b] = [...this.#fingers.values()];
    this.#pinch = { distance: Math.abs(a - b) || 1, mid: (a + b) / 2, view: { ...this.#plot.view } };
    this.#drag = null;
    this.#hit.classList.remove("panning");
    this.clear();
    return true;
  }

  /** Two fingers zoom by their spread, and the tick under their middle stays under it. */
  #pinchMoved(): void {
    const p = this.#pinch;
    const l = this.#plot.layout;
    if (!p || !l || this.#fingers.size !== 2) return;
    const [a, b] = [...this.#fingers.values()];
    const span = ((p.view.end - p.view.start) * p.distance) / (Math.abs(a - b) || 1);
    const width = l.plotRight - l.plotLeft;
    const anchor = p.view.start + ((p.mid - l.plotLeft) / width) * (p.view.end - p.view.start);
    const start = anchor - (((a + b) / 2 - l.plotLeft) / width) * span;
    this.#zoomTo({ start, end: start + span });
  }

  #maybeSweep(): void {
    if (!this.#visible || !this.#loaded || !this.#annotation || !this.#plot.layout) return;
    const id = `${this.#loadKey}\n${this.#annotation.id}`;
    // Resizes and scrolls ask again and again; a plot that never settles, after a failed read,
    // would collect a waiter each time.
    if (this.#awaiting === id) return;
    this.#awaiting = id;
    void this.#plot.whenSettled().then(() => {
      if (this.#awaiting === id) this.#awaiting = "";
      if (!this.#visible || this.#annotation === null || `${this.#loadKey}\n${this.#annotation.id}` !== id) return;
      if (this.#ready !== id) {
        this.#ready = id;
        this.emit("tickwave-ready", { annotation: this.#annotation.id, height: this.offsetHeight });
      }
      if (!this.#sweepWanted || this.#sweepFrame) return;
      if (!parseSweep(this.getAttribute("sweep")) || reducedMotion()) {
        this.#sweepWanted = false;
        this.#plot.reveal = null;
        this.#plot.invalidate();
        return;
      }
      // Below the fold, the sweep waits until the reader scrolls to it.
      if (!this.#inViewport) return;
      this.#sweepWanted = false;
      const t0 = performance.now();
      const step = (now: number) => {
        const layout = this.#plot.layout;
        if (!layout) return;
        const f = Math.min(1, Math.max(0, (now - t0) / SWEEP_MS));
        this.#plot.reveal = f >= 1 ? null : layout.plotLeft + (layout.plotRight - layout.plotLeft) * f;
        this.#plot.drawNow();
        this.#sweepFrame = f >= 1 ? 0 : requestAnimationFrame(step);
      };
      this.#sweepFrame = requestAnimationFrame(step);
    });
  }

  async #renderMeasure(): Promise<void> {
    const seq = ++this.#measureSeq;
    const l = this.#loaded;
    const a = this.#annotation;
    this.#measure.textContent = "";
    const asks = !!l && !!a?.measure && a.end !== null;
    this.#measure.hidden = !asks;
    this.#line.hidden = !asks && !(l && a && this.#readable);
    if (!l || !a?.measure || a.end === null) return;
    const ch = l.info.channels.findIndex((c) => c.name === a.measure!.channel);
    if (ch < 0 || l.info.channels[ch].kind !== "digital") {
      this.fail(new Error(`annotation ${a.id} measures ${a.measure.channel}, which is not a digital channel`));
      return;
    }
    try {
      const edges = await l.source.edges(ch, a.start, a.end);
      if (seq !== this.#measureSeq) return;
      this.#measure.innerHTML = measureText(a.measure.what, a.measure.channel, edges.level, edges.ticks, l.info);
    } catch (e) {
      if (!isAbort(e)) this.fail(e);
    }
  }

  // Pointing, keys and opening ---------------------------------------------------------------------

  /** The view `for` names, if it is one. */
  get #view(): TickwaveView | null {
    const id = this.getAttribute("for");
    const el = id ? document.getElementById(id) : null;
    return el?.localName === "tickwave-view" ? (el as TickwaveView) : null;
  }

  /** The note lit by the pointer or the keys, if any. */
  get lit(): string | null {
    return this.#lit;
  }

  /**
   * `href` with the recording, its sidecar, the annotation `id` and, zoomed in, the `window` as its
   * parameters, the others it has kept. A recording on the same site goes in by its path, relative
   * where it lies under that page's folder; another site's whole.
   */
  #address(id: string): string | null {
    const href = this.getAttribute("href");
    const src = this.getAttribute("src");
    if (href === null || !src) return null;
    const to = new URL(href, document.baseURI);
    const folder = to.pathname.slice(0, to.pathname.lastIndexOf("/") + 1);
    const from = (u: string) => {
      const x = new URL(u, document.baseURI);
      if (x.origin !== to.origin) return x.href;
      return (x.pathname.startsWith(folder) ? x.pathname.slice(folder.length) : x.pathname) + x.search;
    };
    to.searchParams.set("src", from(src));
    const annotations = this.getAttribute("annotations");
    if (annotations) to.searchParams.set("annotations", from(annotations));
    else to.searchParams.delete("annotations");
    to.searchParams.set("annotation", id);
    // Whole ticks that take in the zoomed span; the window reads no ticks before the first.
    const z = this.#zoomed;
    if (z) to.searchParams.set("window", `${Math.max(0, Math.floor(z.start))} ${Math.ceil(z.end)}`);
    else to.searchParams.delete("window");
    return to.href;
  }

  /**
   * The part over the plot: shown where a reader can point at the inset; a link to `href` at the lit
   * note, or the inset's own annotation, unless `for` names a view; one Tab stop, except in a
   * popover, whose opener holds the focus.
   */
  #renderHit(): void {
    const hit = this.#hit;
    const a = this.#annotation;
    hit.hidden = !this.#readable || !a;
    const id = this.#lit ?? a?.id ?? null;
    const view = this.#view;
    const address = id && !view ? this.#address(id) : null;
    if (address) hit.href = address;
    else hit.removeAttribute("href");
    hit.classList.toggle("opens", !!address || !!view);
    const popped = this.closest("[popover]") !== null;
    if (popped) hit.setAttribute("tabindex", "-1");
    else if (address) hit.removeAttribute("tabindex");
    else hit.tabIndex = 0;
    if (address || popped) hit.removeAttribute("role");
    else hit.setAttribute("role", view ? "button" : "group");
    hit.setAttribute("aria-label", a ? (a.label ?? a.id) : "");
    const notes = this.#plot.laneNotes().length > 0;
    hit.setAttribute(
      "aria-description",
      `The up and down arrow keys zoom, and left and right move along it${notes ? "; Shift with left and right steps through its parts" : ""}.`,
    );
  }

  /** A pointer's place in the plot's coordinates. */
  #local(e: PointerEvent | WheelEvent): { x: number; y: number } {
    const box = this.#plot.canvas.getBoundingClientRect();
    return { x: e.clientX - box.left, y: e.clientY - box.top };
  }

  #point(e: PointerEvent): void {
    this.#pointerType = e.pointerType;
    this.#pointAt(this.#local(e));
  }

  /** What lies at `p`: a note lights, a row reads the pulse or the value there. */
  #pointAt(p: { x: number; y: number }): void {
    const l = this.#plot.layout;
    if (!l || !this.#loaded) return;
    this.#pointer = p;
    const hit = this.#plot.hit(p.x, p.y);
    const row = hit.area === "row" ? hit.row : null;
    if (row !== this.#plot.hoverRow) {
      this.#plot.hoverRow = row;
      this.#plot.invalidate();
    }
    if (hit.area === "lane" && hit.note) {
      this.#prober.pointer = null;
      this.#prober.hide();
      this.#light(hit.note.id);
      return;
    }
    this.#light(null);
    this.#prober.pointer = p;
    this.#prober.probe();
  }

  /** Lights note `id`, or none, and reads it in the line; the keys also have it said. */
  #light(id: string | null, said = false): void {
    if (this.#plot.hoverNote !== id) {
      this.#plot.hoverNote = id;
      this.#plot.invalidate();
    }
    if (this.#lit === id && !said) return;
    this.#lit = id;
    this.#renderHit();
    const a = id ? this.#loaded?.annotations.byId.get(id) : undefined;
    if (!a || !this.#loaded) {
      this.#read(null);
      return;
    }
    const items: Item[] = [[["", a.label ?? a.id]]];
    if (a.end !== null) items.push([["figures", formatInterval(a.end - a.start, this.#loaded.info.timebase)]]);
    if (a.paint) items.push([["what", a.paint === "probe" || a.paint === "target" ? `${a.paint} drives` : a.paint]]);
    this.#read(items);
    if (said) this.#said.textContent = items.map((i) => i.map(([, t]) => t).join("")).join(", ");
  }

  /** What the prober reads at the pointer, in the line. */
  #probed(r: Probed | null): void {
    const l = this.#loaded;
    const track = r ? this.#plot.tracks[r.row] : undefined;
    if (!r || !l || !track) {
      if (this.#lit === null) this.#read(null);
      return;
    }
    const name: Item = [["what", track.channel.name]];
    if (r.kind === "text") {
      const items = [name, ...r.lines.map((t): Item => [[t === "high" || t === "low" ? "level" : "", t]])];
      if (r.lines.includes(ZOOM_IN)) items.push([["what", this.#pointerType === "touch" ? "pinch" : `${zoomKey()} + scroll`]]);
      this.#read(items);
      return;
    }
    const f = r.figures;
    const items: Item[] = [[...name, ["", " "], ["level", f.level], ["", " "], ["figures", f.width]]];
    const a = this.#annotation;
    const start = r.pulse.start;
    if (a && typeof start === "number") {
      const d = start - a.start;
      items.push([["", "starts "], ["figures", formatInterval(Math.abs(d), l.info.timebase)], ["", d < 0 ? " before" : " in"]]);
    }
    if (f.period) items.push([["", "period "], ["figures", f.period]]);
    if (f.frequency) items.push([["figures", f.frequency]]);
    if (f.duty) items.push([["", "duty "], ["figures", f.duty]]);
    if (f.inverseWidth) items.push([["", "1/width "], ["figures", f.inverseWidth]]);
    this.#read(items);
  }

  /** The reading in the line, in place of the measure, as many whole items as fit; null for none. */
  #read(items: readonly Item[] | null): void {
    const r = this.#reading;
    if (!items) {
      this.#line.classList.remove("reads");
      r.replaceChildren();
      return;
    }
    r.replaceChildren(
      ...items.map((item) => {
        const span = document.createElement("span");
        for (const [cls, text] of item) {
          const piece = cls ? Object.assign(document.createElement("span"), { className: cls, textContent: text }) : text;
          if (cls === "level") (piece as HTMLElement).replaceChildren(Object.assign(document.createElement("span"), { textContent: text }));
          span.append(piece);
        }
        return span;
      }),
    );
    this.#line.classList.add("reads");
    while (r.childElementCount > 1 && this.#line.scrollWidth > this.#line.clientWidth) r.lastElementChild!.remove();
  }

  /**
   * Answers an arrow key as the inset's own: up and down zoom in and out, left and right move along
   * the span, and Shift with left or right steps through the notes. True if it answered.
   */
  arrowKey(e: KeyboardEvent): boolean {
    if (e.altKey || e.ctrlKey || e.metaKey) return false;
    const { start, end } = this.#plot.view;
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      const dir = e.key === "ArrowRight" ? 1 : -1;
      if (e.shiftKey) this.#step(dir);
      else this.#panBy(dir * (end - start) * 0.125);
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      // About the lit note, which stays where it is, or else the middle.
      const a = this.#lit ? this.#loaded?.annotations.byId.get(this.#lit) : undefined;
      const at = a ? (a.start + (a.end ?? a.start)) / 2 : (start + end) / 2;
      this.#zoomBy(e.key === "ArrowUp" ? 1 / 1.6 : 1.6, at);
    } else return false;
    return true;
  }

  /** Lights the next note in time (`dir` 1) or the one before, and has it said. */
  #step(dir: 1 | -1): void {
    const l = this.#loaded;
    if (!l) return;
    const notes = [...this.#plot.laneNotes()].sort((x, y) => x.start - y.start || (x.end ?? x.start) - (y.end ?? y.start));
    if (notes.length === 0) return;
    const at = notes.findIndex((a) => a.id === this.#lit);
    const n = notes[at < 0 ? (dir > 0 ? 0 : notes.length - 1) : Math.max(0, Math.min(notes.length - 1, at + dir))];
    this.#pointer = null;
    this.#prober.pointer = null;
    this.#prober.hide();
    // Zoomed in, the span moves to the note: at its width, or wider to take the note in whole.
    const v = this.#plot.view;
    const end = n.end ?? n.start;
    if (this.#zoomed && (n.start < v.start || end > v.end)) {
      const span = Math.max(v.end - v.start, (end - n.start) * 1.25);
      const mid = (n.start + end) / 2;
      this.#zoomTo({ start: mid - span / 2, end: mid + span / 2 });
    }
    this.#light(n.id, true);
  }

  /** Lights nothing and reads nothing. */
  clear(): void {
    this.#pointer = null;
    this.#prober.pointer = null;
    this.#prober.hide();
    if (this.#plot.hoverRow !== null) {
      this.#plot.hoverRow = null;
      this.#plot.invalidate();
    }
    this.#light(null);
    this.#said.textContent = "";
  }

  /** Opens `for`'s view at the lit note or the inset's annotation, over the zoomed span if there is
   * one; true if it did. */
  #openView(): boolean {
    const view = this.#view;
    const id = this.#lit ?? this.#annotation?.id;
    if (!view || !id) return false;
    void view.reveal(id, this.zoomed);
    return true;
  }

  #clicked(e: MouseEvent): void {
    const touch = this.#touch;
    this.#touch = false;
    if (this.#dragged) {
      this.#dragged = false;
      e.preventDefault();
      return;
    }
    // A tap lights a note or reads a pulse; a tap on the note already lit opens at it.
    if (touch && (this.#lit === null || this.#lit !== this.#litAtTouch)) {
      e.preventDefault();
      return;
    }
    // A link goes where its address says, a new tab included; a view opens in place.
    if (this.#openView()) e.preventDefault();
  }

  #keyed(e: KeyboardEvent): void {
    if (this.arrowKey(e)) e.preventDefault();
    else if (e.key === "Escape" && this.#lit !== null) {
      e.preventDefault();
      e.stopPropagation();
      this.clear();
    } else if ((e.key === "Enter" || e.key === " ") && !this.#hit.hasAttribute("href")) {
      if (this.#openView()) e.preventDefault();
    }
  }

  #renderKey(): void {
    const l = this.#loaded;
    const a = this.#annotation;
    this.#key.textContent = "";
    if (!l || !a || !this.palette) return;
    const names = new Set(this.#plot.tracks.map((t) => t.channel.name));
    const roles = new Set<string>();
    for (const b of l.annotations.list) {
      if (b.paint === null || b.end === null || b.end <= a.start || b.start >= (a.end ?? a.start)) continue;
      if ([...names].some((n) => covers(b, n))) roles.add(b.paint);
    }
    for (const r of [...roles].sort()) {
      const item = document.createElement("span");
      const swatch = document.createElement("i");
      swatch.style.background = this.palette.role(r);
      // As heavy as the role's lines, so the key tells the roles apart without colour too.
      if (ROLE_WEIGHT[r]) swatch.className = "heavy";
      item.append(swatch, r === "probe" || r === "target" ? `${r} drives` : r);
      this.#key.append(item);
    }
  }
}

/** The `measure` readout as HTML: empty when the span says nothing. */
export function measureText(what: MeasureKind, channel: string, level: 0 | 1, ticks: Float64Array, info: RecordingInfo): string {
  const r = measureReading(what, level, ticks, info.timebase);
  if (r === null) return "";
  return `<span class="what">${esc(channel)}</span> ${esc(r.value)} <span class="what">${esc(r.note)}</span>`;
}
