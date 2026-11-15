// <tickwave-view>: the full capture. Collapsed to its title and facts until `open`; open, a time axis, one row
// a channel and an annotation lane, with zoom, pan, cursors, a selection and its readouts.

import { indexAnnotations, ofKind, type Annotation } from "../annotations.ts";
import { parseChannels, parseWindow, VIEW_ATTRIBUTES } from "../attributes.ts";
import { reducedMotion } from "../face.ts";
import { absolute, events as registryEvents, infoFor, peekInfo } from "../registry.ts";
import { clockNote, countEdges, formatClockPeriod, formatClockRate, formatCount, formatInterval, formatMarks, formatRate, formatValue, periodStats } from "../readouts.ts";
import { completePulse, knownSpan, pulseFigures, pulseFromEdges, type Pulse } from "../probe.ts";
import { Prober, type Probed } from "../prober.ts";
import { lowerBound } from "../search.ts";
import { channelIndices, Plot } from "../render/plot.ts";
import { clampRange, fitRange, moveRange, scaleLimits, zoomRange, type ScaleLimits, type ValueRange } from "../render/scale.ts";
import { channelSampleTick, channelStart, inChannel, isAbort, named, readingStep, sampleAtOrBefore, spanned } from "../source.ts";
import { parseTime, toTicks, type TimeSpec } from "../time.ts";
import { DRAG_PX, TickwaveElement, loadAnnotations, loadHead, zoomKey, type Loaded } from "./base.ts";
import { ShapedSource } from "../shaped-source.ts";
import { SUMMARY_CSS } from "./summary-style.ts";
import { shownExtent, shownStart, summaryFacts } from "../summary.ts";

/** The width of a row's move grip, at the names column's left, while the view shapes. */
const GRIP_PX = 14;

const CSS = `
:host { display: flex; flex-direction: column; }
.box { background: var(--surface, Canvas); border: 1px solid var(--hair, GrayText); border-radius: 10px; }
${SUMMARY_CSS}.viewer { display: flex; flex-direction: column; flex: 1 1 auto; min-height: 0; box-sizing: border-box; padding: 6px 0 8px;
  border-top: 0; border-radius: 0 0 10px 10px; }
.toolbar { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 6px;
  padding: 0 10px 6px 14px; }
.toolbar .group { display: flex; gap: 4px; align-items: center; }
.plot { position: relative; flex: 1 1 auto; min-height: 0; touch-action: pan-y; cursor: crosshair; }
.plot:focus-visible { outline-offset: -2px; border-radius: 4px; }
.plot.panning { cursor: grabbing; }
canvas { display: block; }
.lane-says { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
.readout { display: grid; gap: 3px; padding: 6px 14px 0; font-size: 13px; color: var(--ink-2, GrayText); }
.readout:empty { display: none; }
.readout .line { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 14px; }
.readout .figures { color: var(--ink, CanvasText); }
.readout label { display: inline-flex; gap: 6px; align-items: baseline; }
.readout select { min-height: 22px; padding: 1px 4px; font-size: 12px; }
.steps { display: flex; gap: 4px; align-items: center; }
.steps .count { min-width: 5.5em; text-align: center; color: var(--ink-2, GrayText); font-size: 13px; }
.hint { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); pointer-events: none;
  padding: 6px 12px; border-radius: 6px; background: var(--ink, CanvasText); color: var(--page, Canvas);
  font-size: 13px; white-space: nowrap; opacity: 0; transition: opacity .15s; }
.hint.shown { opacity: 1; }
@media (prefers-reduced-motion: reduce) { .hint { transition: none; } }
.reading { position: absolute; z-index: 1; pointer-events: none; padding: 3px 7px; border-radius: 5px;
  background: var(--raised, Canvas); border: 1px solid var(--hair, GrayText); color: var(--ink, CanvasText);
  font-size: 12px; line-height: 1.4; white-space: nowrap; font-variant-numeric: tabular-nums; }
.reading .more { color: var(--ink-2, GrayText); }
.scale { position: absolute; left: 0; cursor: grab; touch-action: none; }
.scale.dragging { cursor: grabbing; }
.scale:focus-visible { outline-offset: -2px; border-radius: 4px; }
.row-handle { position: absolute; left: 0; height: 8px; margin-top: -4px; cursor: row-resize; touch-action: none; }
@media (pointer: coarse) { .row-handle { height: 24px; margin-top: -12px; } }
.row-handle::after { content: ""; position: absolute; left: 4px; right: 4px; top: 50%; height: 2px; margin-top: -1px;
  border-radius: 1px; background: var(--ink, CanvasText); opacity: 0; }
.row-handle:hover::after, .row-handle:focus-visible::after, .row-handle.dragging::after { opacity: 1; }
.grip { position: absolute; left: 0; width: ${GRIP_PX}px; cursor: grab; touch-action: none; border-radius: 4px; }
/* Three dots a row, in the middle of it, so each row's grip stands apart from the next. */
.grip::after { content: ""; position: absolute; left: 0; right: 0; top: 50%; height: 16px; margin-top: -8px;
  background: radial-gradient(circle, var(--ink-2, GrayText) 1.2px, transparent 1.5px) center / 6px 6px repeat-y; }
.grip:hover, .grip.dragging { background-color: var(--wash, color-mix(in srgb, CanvasText 12%, transparent)); }
.grip.dragging { cursor: grabbing; }
.grip:focus-visible { outline-offset: -2px; }
.trim { position: absolute; width: 8px; margin-left: -4px; cursor: ew-resize; touch-action: none; }
@media (pointer: coarse) { .trim { width: 24px; margin-left: -12px; } }
.trim::after { content: ""; position: absolute; left: 50%; top: 2px; bottom: 2px; width: 2px; margin-left: -1px;
  border-radius: 1px; background: var(--ink, CanvasText); opacity: .55; }
.trim:hover::after, .trim:focus-visible::after, .trim.dragging::after { opacity: 1; }
.trim:focus-visible { outline: none; }
`;

const HTML = `
<div class="summary" part="summary">
  <span class="title" part="title"><slot name="title"></slot></span><span class="facts" part="facts"><slot name="facts"></slot></span>
  <button type="button" class="toggle" part="button open-button">Open</button>
</div>
<div class="viewer box" part="viewer" hidden>
  <div class="toolbar" part="toolbar">
    <div class="group">
      <button type="button" class="zoom-out" part="button" aria-label="Zoom out">−</button>
      <button type="button" class="zoom-in" part="button" aria-label="Zoom in">+</button>
      <button type="button" class="fit" part="button">Fit</button>
    </div>
    <div class="steps" part="steps" hidden>
      <select class="kind" aria-label="Annotation kind"></select>
      <button type="button" class="prev" part="button" aria-label="Previous">‹</button>
      <span class="count figures" aria-live="polite"></span>
      <button type="button" class="next" part="button" aria-label="Next">›</button>
    </div>
  </div>
  <div class="plot" part="plot" tabindex="0" role="group" aria-roledescription="capture"><canvas aria-hidden="true"></canvas><div class="hint" part="hint" aria-hidden="true"></div><div class="reading" part="reading" aria-hidden="true" hidden></div></div>
  <div class="readout" part="readout" aria-live="polite"></div>
</div>
<div class="notice" part="notice" role="status" hidden></div>`;

/** Between the hover label and the row it reads, in CSS pixels. */
const LABEL_GAP = 6;

/** What a kept annotation lane says once its annotations are in and none falls on a shown channel. */
const EMPTY_LANE = "No annotations on the channels shown";

const SNAP_PX = 6;
/** The heights a reader can give a row, in CSS pixels. */
const ROW_LIMITS = { digital: [24, 400], analog: [40, 600] } as const;
/** A handle's arrow keys move it this far, or the second with Shift. */
const ROW_STEP = [4, 16] as const;

interface Drag {
  readonly id: number;
  readonly x: number;
  readonly y: number;
  readonly area: "row" | "axis" | "lane" | "none";
  readonly row: number;
  readonly view: { start: number; end: number };
  /** The values of a zoomed-in analog row under a mouse or pen, which the drag moves too. */
  readonly values: ValueRange | null;
  moved: boolean;
}

/** A channel's shape in the convert flow: the ticks it is moved by, and the part of its own data kept. */
export interface ChannelShape {
  readonly offset: number;
  readonly keep: { readonly from: number; readonly to: number } | null;
}

/** Channels that move together, under a head row with the group's name: one input of a merge. */
export interface ChannelGroup {
  readonly name: string;
  /** By channel name. */
  readonly channels: readonly string[];
}

export class TickwaveView extends TickwaveElement {
  static observedAttributes = VIEW_ATTRIBUTES;
  readonly #plot: Plot;
  readonly #titleSlot: HTMLSlotElement;
  readonly #factsSlot: HTMLSlotElement;
  readonly #toggle: HTMLButtonElement;
  readonly #viewer: HTMLElement;
  readonly #plotBox: HTMLElement;
  readonly #readout: HTMLElement;
  readonly #steps: HTMLElement;
  readonly #kind: HTMLSelectElement;
  #loaded: Loaded | null = null;
  #loading: Promise<Loaded | null> | null = null;
  /** The recording's load expected annotations, so their lane keeps its room for it. */
  #laneKept = false;
  /** The recording's annotations are in, or failed: what `tickwave-ready` waits for besides
   * the rows. */
  #annotated = false;
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
  #summary: Summary | null = null;
  /** The hand-written sidecar as text, from a page editing it; null to read the sidecar's file. */
  #annotationsText: string | null = null;
  /** Counts the texts handed in: one handed in later makes an earlier one's annotations stale. */
  #textSeq = 0;
  /** The sidecar's problems last reported, forgotten when another sidecar replaces it. */
  #sidecarProblems: string[] = [];
  /** The recording as loaded. `#loaded` is it moved by its offsets while the view shapes it. */
  #asLoaded: Loaded | null = null;
  #shaping = false;
  #shaped: ShapedSource | null = null;
  /** Each channel's kept span, by channel index, in its own ticks; absent: all of it. */
  #keep = new Map<number, { from: number; to: number }>();
  /** A shape asked for before the recording arrived, applied once it has. */
  #pendingShape: Record<string, ChannelShape> | null = null;
  #groups: readonly ChannelGroup[] = [];
  /** Each grouped channel's group, by channel index. */
  #groupOf = new Map<number, string>();
  /** Each group's alignment in ticks, positive later; absent: 0. Every channel of the group moves
   * by it, on top of its own offset. */
  #aligns = new Map<string, number>();
  /** Alignments asked for before the recording arrived, applied once it has. */
  #pendingAligns: Record<string, number> | null = null;
  /** One a group while the view shapes: a drag sideways moves the whole group. */
  #heads = new Map<string, HTMLElement>();
  /** By channel index, so a channel's grip follows its row when the rows change order. */
  #grips = new Map<number, HTMLElement>();
  /** Two a channel: where its kept span starts and ends. */
  #trims = new Map<number, [HTMLElement, HTMLElement]>();
  /** The rows last drawn, by channel index: a change of order alone keeps the view's place. */
  #rowSet = "";
  /** The mismatches between the page's summary and the recording's, warned of once each. */
  #warned = new Set<string>();
  #resize: ResizeObserver;
  /** One a row, on its lower edge in the names column. */
  #handles: HTMLElement[] = [];
  /** One an analog row, over its scale; null for a digital row. */
  #scales: (HTMLElement | null)[] = [];
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
    this.#titleSlot = this.root.querySelector("slot[name=title]")!;
    this.#factsSlot = this.root.querySelector("slot[name=facts]")!;
    for (const slot of [this.#titleSlot, this.#factsSlot]) slot.addEventListener("slotchange", () => this.#renderSummary());
    this.#toggle = this.root.querySelector(".toggle")!;
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
    this.#prober = new Prober(
      this.#plot,
      () => this.#loaded,
      (r) => this.#shown(r),
      () => !!this.#drag?.moved || !!this.#pinch,
    );
    this.#plot.onDraw = () => {
      this.#drawn();
      this.#placeTrims();
      this.#prober.probeIfMoved();
    };
    this.#resize = new ResizeObserver(() => this.#sized());
    const on = (sel: string, f: () => void) => this.root.querySelector(sel)!.addEventListener("click", f);
    on(".toggle", () => {
      this.open = !this.open;
      this.update();
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

  /**
   * The recording's own title and facts, and whether the page's slotted text matches them. Null
   * until the view has the head and has compared.
   */
  get summary(): Summary | null {
    return this.#summary;
  }

  /** The visible span in ticks. */
  get window(): { start: number; end: number } {
    return { ...this.#plot.view };
  }

  /** Cursors A and B in ticks; null where one is not placed. */
  get cursors(): { a: number | null; b: number | null } {
    return { ...this.#plot.cursors };
  }

  /** The selected span in ticks, or null. */
  get selection(): { start: number; end: number } | null {
    const s = this.#plot.selection;
    return s ? { start: Math.round(Math.min(s.start, s.end)), end: Math.round(Math.max(s.start, s.end)) } : null;
  }

  /**
   * The sidecar's hand-written file as text, in place of the file at `annotations`, whose
   * includes it still reads. Set by a page that edits the annotations: the view redraws them and
   * keeps its place, cursors and selection, and keeps the annotation lane while it is set. Null
   * goes back to the file.
   */
  get annotationsText(): string | null {
    return this.#annotationsText;
  }

  set annotationsText(text: string | null) {
    if (text === this.#annotationsText) return;
    this.#annotationsText = text;
    this.#plot.keepLane = text !== null || this.#laneKept;
    void this.#reannotate();
  }

  /**
   * Whether the view shapes its recording, as the convert flow does before the recording is written
   * again: each row gets a grip in the names column that moves the channel in time (sideways) or
   * among the rows (up or down), and handles at its data's ends that trim it. Nothing changes the
   * recording; `shape` says what the reader did.
   */
  get shaping(): boolean {
    return this.hasAttribute("shaping");
  }

  set shaping(v: boolean) {
    this.toggleAttribute("shaping", v);
  }

  /**
   * Each channel's shape, by its name: `offset`, the ticks it is moved by, and `keep`, the part of
   * its own data kept (`{from, to}` in the recording's ticks, before the move), or null for all of
   * it. A group's alignment is not in the offset: see `aligns`. Setting it moves and trims the
   * channels it names. The rows' order is `channels`, by name, which a move up or down rewrites.
   */
  get shape(): Record<string, ChannelShape> {
    const l = this.#asLoaded;
    if (!l) return {};
    return Object.fromEntries(l.info.channels.map((c, i) => [c.name, { offset: this.#own(i), keep: this.#keep.get(i) ?? null }]));
  }

  set shape(v: Record<string, ChannelShape>) {
    const l = this.#asLoaded;
    if (!l || !this.#shaped) {
      this.#pendingShape = { ...this.#pendingShape, ...v };
      return;
    }
    for (const [name, s] of Object.entries(v)) {
      const i = l.info.channels.findIndex((c) => c.name === name);
      if (i < 0) throw new RangeError(`no channel ${name}`);
      this.#moveChannel(i, s.offset);
      this.#setKeep(i, s.keep);
    }
    this.#placeShaping();
  }

  /**
   * Channels that belong together, such as each input of a merge, each under a head row with its
   * name. A channel in no group has no head. While the view shapes, a head's grip moves its whole
   * group in time (`aligns`), and a channel's grip moves its row only among its group's rows.
   */
  get groups(): readonly ChannelGroup[] {
    return this.#groups;
  }

  set groups(v: readonly ChannelGroup[]) {
    this.#groups = v.map((g) => ({ name: g.name, channels: [...g.channels] }));
    this.#regroup();
    this.#layout(true);
  }

  /** Each group's alignment, by its name: the ticks every channel in it is moved by, positive later,
   * on top of each channel's own offset. Setting it moves the groups it names. */
  get aligns(): Record<string, number> {
    return Object.fromEntries(this.#groups.map((g) => [g.name, this.#aligns.get(g.name) ?? 0]));
  }

  set aligns(v: Record<string, number>) {
    if (!this.#asLoaded || !this.#shaped) {
      this.#pendingAligns = { ...this.#pendingAligns, ...v };
      return;
    }
    for (const [name, ticks] of Object.entries(v)) {
      if (!this.#groups.some((g) => g.name === name)) throw new RangeError(`no group ${name}`);
      this.#moveGroup(name, ticks);
    }
    this.#placeShaping();
  }

  /** Each channel's group from `groups`, by channel index, for the recording shown. */
  #regroup(): void {
    const l = this.#asLoaded;
    this.#groupOf.clear();
    if (l) {
      for (const g of this.#groups) {
        for (const name of g.channels) {
          const i = l.info.channels.findIndex((c) => c.name === name);
          if (i < 0) this.fail(new Error(`group ${g.name}: no channel ${name}`));
          else this.#groupOf.set(i, g.name);
        }
      }
    }
    this.#plot.groups = new Map(this.#groupOf);
    for (const name of this.#aligns.keys()) if (!this.#groups.some((g) => g.name === name)) this.#aligns.delete(name);
  }

  /** The annotations again, from the text or the file, over the recording already shown. */
  async #reannotate(): Promise<void> {
    const l = this.#loaded;
    // A load under way reads the text once it has the recording.
    if (!l) return;
    const seq = ++this.#textSeq;
    const annotations = await loadAnnotations(l.src, this.getAttribute("annotations"), this.#annotationsText);
    if (seq !== this.#textSeq || this.#loaded !== l) return;
    this.#plot.emptyLane = annotations.errors.length === 0 ? EMPTY_LANE : null;
    this.#loaded = { ...l, annotations };
    if (this.#asLoaded) this.#asLoaded = { ...this.#asLoaded, annotations };
    this.#reportSidecar(annotations.errors);
    this.#apply();
  }

  #reportSidecar(errors: readonly string[]): void {
    for (const m of this.#sidecarProblems) this.forgetProblem(m);
    this.#sidecarProblems = [...errors];
    for (const e of errors) this.fail(named("TickwaveSidecarError", e));
  }

  protected paletteChanged(): void {
    this.#plot.palette = this.palette;
    if (this.#loaded) this.#layout(true);
  }

  /** The canvas takes the new face in the frame the page's colours change, not the next. */
  protected override faceTurned(): void {
    super.faceTurned();
    this.#plot.drawNow();
  }

  protected update(): void {
    if (this.shaping !== this.#shaping) this.#setShaping(this.shaping);
    const open = this.open;
    this.#viewer.hidden = !open;
    this.#renderSummary();
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
      return;
    }
    if (!src) return;
    if (key !== this.#loadKey) {
      this.#loadKey = key;
      this.#loaded = null;
      this.#readyFired = false;
      this.#annotated = false;
      this.#plot.emptyLane = null;
      this.clearProblems();
      const text = this.#annotationsText;
      // The recording draws from its footer while its annotations are read, which a bundled
      // sidecar's are from the file, unless the view opens at one of them. Their lane keeps its
      // room from the first draw where some are expected, so nothing moves when they land.
      const at = parseWindow(this.getAttribute("window"))?.kind === "annotation";
      this.#loading = loadHead(src).then(
        async (head) => {
          if (this.#loadKey !== key) return null;
          this.#laneKept = text !== null || annotations !== null || head.info.bundled === true;
          this.#plot.keepLane = this.#laneKept;
          const first = (l: Loaded) => {
            this.#loaded = this.#wrap(l);
            this.#windowAttr = null;
            this.#apply();
            if (this.#shaping) this.#applyPending();
          };
          if (!at) first({ ...head, annotations: indexAnnotations([]) });
          // A sidecar's text set meanwhile reads its own annotations over the drawn recording.
          const seq = this.#textSeq;
          const list = await loadAnnotations(head.src, annotations, text);
          if (this.#loadKey !== key) return null;
          this.#annotated = true;
          if (!at && this.#textSeq !== seq) return this.#loaded;
          this.#plot.emptyLane = list.errors.length === 0 ? EMPTY_LANE : null;
          if (at) first({ ...head, annotations: list });
          else {
            this.#loaded = { ...this.#loaded!, annotations: list };
            if (this.#asLoaded) this.#asLoaded = { ...this.#asLoaded, annotations: list };
            this.#apply();
          }
          this.#reportSidecar(list.errors);
          if (this.#annotationsText !== text) void this.#reannotate();
          return this.#loaded;
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

  /**
   * The view's own title and facts, under whatever text the page slotted over them. Once the head is
   * in, a page's text that says otherwise is warned of in the console, since the page shows it.
   */
  #renderSummary(): void {
    const src = this.#src;
    const action = this.open ? "Close" : "Open";
    this.#toggle.textContent = action;
    if (!src) {
      this.#setSummary(null);
      return;
    }
    // Collapsed, the view shows what is here; the head arrives once the view scrolls into sight.
    const info = peekInfo(src);
    const title = info?.title ?? decodeURIComponent(src.pathname.split("/").pop() ?? "");
    const facts = info ? summaryFacts(info, channelIndices(info, parseChannels(this.getAttribute("channels"))).indices) : [];
    const factsText = facts.map(([, text]) => text).join("; ");
    // Only when they change: new fallback text in a slot with nothing assigned is a slotchange,
    // which renders the summary again.
    if (this.#titleSlot.textContent !== title) this.#titleSlot.textContent = title;
    if (this.#factsSlot.textContent !== factsText) {
      this.#factsSlot.replaceChildren(
        ...facts.flatMap(([name, text], i) => [...(i > 0 ? [part("fact-separator", "; ")] : []), part(`fact ${name}`, text)]),
      );
    }
    const pageTitle = slotted(this.#titleSlot);
    // A page with several views has several of these buttons: each names its recording.
    this.#toggle.setAttribute("aria-label", `${action} ${pageTitle ?? title}`);
    if (!info) {
      this.#setSummary(null);
      return;
    }
    let matches = true;
    for (const [what, page, own] of [
      ["title", pageTitle, title],
      ["facts", slotted(this.#factsSlot), factsText],
    ] as const) {
      if (page === null || page === collapse(own)) continue;
      matches = false;
      const warning = `tickwave-view${this.id ? `#${this.id}` : ""} src="${this.getAttribute("src")}": the page's ${what} "${page}" ${what === "title" ? "differs" : "differ"} from the recording's, "${own}"`;
      if (!this.#warned.has(warning)) {
        this.#warned.add(warning);
        console.warn(warning);
      }
    }
    this.#setSummary({ title, facts: factsText, matches });
  }

  #setSummary(s: Summary | null): void {
    const was = this.#summary;
    if (s === was || (s && was && s.title === was.title && s.facts === was.facts && s.matches === was.matches)) return;
    this.#summary = s;
    if (s) this.emit("tickwave-summary", s);
  }

  #apply(): void {
    const l = this.#loaded;
    if (!l) return;
    this.#renderSummary();
    this.#plotBox.setAttribute(
      "aria-label",
      `${l.info.title}. Left and right arrows pan, up and down zoom, Home fits the capture; Enter or a click places a cursor, drag on the time axis to select.`,
    );
    const { indices, missing } = channelIndices(l.info, parseChannels(this.getAttribute("channels")));
    for (const m of missing) this.fail(new Error(`no channel ${m}`));
    const recordingKey = `${l.src.href}\n${indices.join(" ")}`;
    const fresh = recordingKey !== this.#recordingKey;
    if (fresh) {
      this.#recordingKey = recordingKey;
      const rows = `${l.src.href}\n${[...indices].sort((a, b) => a - b).join(" ")}`;
      this.#plot.setRecording(l.source, l.info, indices);
      // The same rows in another order, as a move up or down makes: the view keeps its place.
      if (rows !== this.#rowSet) this.#plot.view = { start: this.#start(), end: this.#extent() };
      this.#rowSet = rows;
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
    this.#describeLane();
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

  /**
   * The plot group's description: the lane's words while the plot draws them, read when assistive
   * tech reaches the plot and never announced. The element exists only then. It sits beside the
   * group, and assistive tech skips it where it sits, so the words are read once, with the group.
   */
  #describeLane(): void {
    const says = this.#plot.laneSays();
    let el = this.root.querySelector<HTMLElement>(".lane-says");
    if (says === null) {
      el?.remove();
      this.#plotBox.removeAttribute("aria-describedby");
      return;
    }
    if (!el) {
      el = document.createElement("span");
      el.className = "lane-says";
      el.id = "lane-says";
      el.setAttribute("aria-hidden", "true");
      this.#plotBox.after(el);
    }
    el.textContent = says;
    this.#plotBox.setAttribute("aria-describedby", el.id);
  }

  /** A new recording starts clean: the last one's cursors, selection and place among its
   * annotations mean nothing in it. */
  #forget(): void {
    this.#setCursors(null, null);
    this.#plot.selection = null;
    this.#plot.focus = null;
    this.#plot.heights.clear();
    this.#plot.ranges.clear();
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
    this.#describeLane();
    this.#placeHandles();
    this.#placeScales();
    this.#placeShaping();
    this.#clampView();
    this.#plot.invalidate();
  }

  // Shaping -------------------------------------------------------------------------------------

  /** `l`, moved by its offsets while the view shapes it. Another recording starts unmoved. */
  #wrap(l: Loaded): Loaded {
    const fresh = this.#asLoaded?.source !== l.source;
    if (fresh) {
      this.#shaped = null;
      this.#keep.clear();
      this.#aligns.clear();
      this.#plot.kept.clear();
    }
    this.#asLoaded = l;
    if (fresh) this.#regroup();
    if (!this.#shaping) return l;
    this.#shaped ??= new ShapedSource(l.source, l.info);
    return { ...l, source: this.#shaped, info: this.#shaped.shapedInfo };
  }

  #setShaping(on: boolean): void {
    this.#shaping = on;
    this.#plot.grip = on ? GRIP_PX : 0;
    const l = this.#asLoaded;
    if (l) {
      this.#loaded = this.#wrap(l);
      this.#plot.setRecording(this.#loaded.source, this.#loaded.info, this.#plot.tracks.map((t) => t.index));
      for (const i of l.info.channels.keys()) this.#showKeep(i);
      if (on) this.#applyPending();
    }
    this.#layout(true);
    this.#placeShaping();
  }

  /** Channel `i`'s group's alignment. */
  #alignOf(i: number): number {
    const g = this.#groupOf.get(i);
    return g === undefined ? 0 : (this.#aligns.get(g) ?? 0);
  }

  /** Channel `i`'s own offset, without its group's alignment. */
  #own(i: number): number {
    return (this.#shaped?.offsets[i] ?? 0) - this.#alignOf(i);
  }

  /** Whether channel `i` is derived from others: it has no grip or trim handles, and moves and
   * trims with its first operand, whose samples it sits on. */
  #derived(i: number): boolean {
    const c = this.#asLoaded?.info.channels[i];
    return c?.kind === "analog" && c.product !== undefined;
  }

  /** The derived channels whose first operand is channel `i`. */
  #followers(i: number): number[] {
    const channels = this.#asLoaded?.info.channels ?? [];
    const name = channels[i]?.name;
    return channels.flatMap((c, j) => (c.kind === "analog" && c.product?.[0] === name ? [j] : []));
  }

  /** Alignments and shapes asked for before the recording was here to shape. */
  #applyPending(): void {
    const aligns = this.#pendingAligns;
    const shape = this.#pendingShape;
    this.#pendingAligns = null;
    this.#pendingShape = null;
    try {
      if (aligns) this.aligns = aligns;
      if (shape) this.shape = shape;
    } catch (e) {
      this.fail(e);
    }
  }

  /** Moves channel `i`, and the channels derived from it, by `by` ticks more, wherever they are. */
  #shift(i: number, by: number): void {
    const s = this.#shaped;
    const l = this.#loaded;
    if (!s || !l || by === 0) return;
    for (const j of [i, ...this.#followers(i)]) {
      s.setOffset(j, s.offsets[j] + by);
      this.#loaded = { ...this.#loaded!, info: s.shapedInfo };
      this.#plot.moveChannel(s.shapedInfo, j, by);
      this.#showKeep(j);
    }
  }

  /** Moves channel `i` to `to` ticks of its own from where the recording has it. */
  #moveChannel(i: number, to: number): void {
    if (!this.#shaped || !Number.isFinite(to) || this.#derived(i)) return;
    this.#shift(i, Math.round(to) - this.#own(i));
  }

  /** Aligns group `name` at `to` ticks from where the recording has it: every channel in it moves. */
  #moveGroup(name: string, to: number): void {
    if (!this.#shaped || !Number.isFinite(to)) return;
    const by = Math.round(to) - (this.#aligns.get(name) ?? 0);
    if (by === 0) return;
    this.#aligns.set(name, Math.round(to));
    for (const [i, g] of this.#groupOf) if (g === name && !this.#derived(i)) this.#shift(i, by);
  }

  /** Keeps `keep` of channel `i`'s own data, within it and at least a tick long; null keeps all. */
  #setKeep(i: number, keep: { from: number; to: number } | null): void {
    const c = this.#asLoaded?.info.channels[i];
    if (!c || this.#derived(i)) return;
    const lo = channelStart(c);
    let kept = keep;
    if (kept) {
      const from = Math.min(Math.max(Math.round(kept.from), lo), c.end - 1);
      const to = Math.max(Math.min(Math.round(kept.to), c.end), from + 1);
      kept = from <= lo && to >= c.end ? null : { from, to };
    }
    for (const j of [i, ...this.#followers(i)]) {
      if (kept) this.#keep.set(j, kept);
      else this.#keep.delete(j);
      this.#showKeep(j);
    }
  }

  /** Channel `i`'s kept span where the plot draws it: moved with the channel. */
  #showKeep(i: number): void {
    const k = this.#shaping ? this.#keep.get(i) : undefined;
    const d = this.#shaped?.offsets[i] ?? 0;
    if (k) this.#plot.kept.set(i, { from: k.from + d, to: k.to + d });
    else this.#plot.kept.delete(i);
    this.#plot.invalidate();
  }

  /** Puts channel `channel`'s row at `to` among the rows, by rewriting `channels`; a grouped channel
   * stays among its group's rows. */
  #reorder(channel: number, to: number): void {
    const l = this.#loaded;
    if (!l) return;
    const order = this.#plot.tracks.map((t) => t.index).filter((i) => i !== channel);
    const g = this.#groupOf.get(channel);
    let [lo, hi] = [0, order.length];
    if (g !== undefined) {
      const at = this.#plot.tracks.findIndex((t) => t.index === channel);
      const mates = order.flatMap((i, k) => (this.#groupOf.get(i) === g ? [k] : []));
      [lo, hi] = mates.length > 0 ? [mates[0], mates[mates.length - 1] + 1] : [at, at];
    }
    order.splice(Math.max(lo, Math.min(hi, to)), 0, channel);
    this.setAttribute("channels", order.map((i) => l.info.channels[i].name).join(" "));
  }

  /** Says the reader moved, trimmed or reordered channel `i`, or aligned group `group`: the whole
   * shape, the alignments, and what changed. */
  #shapeChanged(change: "move" | "trim" | "order", i: number): void;
  #shapeChanged(change: "align", group: string): void;
  #shapeChanged(change: "move" | "trim" | "order" | "align", which: number | string): void {
    const l = this.#asLoaded;
    if (!l) return;
    // A move up or down has rewritten `channels`, which the rows take up at the next update.
    const order = parseChannels(this.getAttribute("channels")) ?? this.#plot.tracks.map((t) => l.info.channels[t.index].name);
    const what = typeof which === "string" ? { group: which } : { channel: l.info.channels[which].name };
    this.emit("tickwave-shape", { shape: this.shape, aligns: this.aligns, channels: order, change, ...what });
  }

  /**
   * Channel `i`'s offset `to`, or the one that puts the edge of it shown nearest another row's shown
   * edge on that edge, where the two are within a few pixels. Rows read at a scale with no edges in
   * hand have none to snap to.
   */
  #snapMove(i: number, to: number): number {
    return to + (this.#snap(new Set([i]), to - this.#own(i)) ?? 0);
  }

  /** Group `name`'s alignment `to`, or the one that puts an edge of it shown nearest another
   * group's shown edge, or an ungrouped row's, on that edge. */
  #snapGroup(name: string, to: number): number {
    const mine = new Set([...this.#groupOf].flatMap(([i, g]) => (g === name ? [i] : [])));
    return to + (this.#snap(mine, to - (this.#aligns.get(name) ?? 0)) ?? 0);
  }

  /** The least further move that lands an edge shown of the rows `moving`, moved `by` ticks more
   * than now, on an edge shown of another row within a few pixels; null where none is that near. */
  #snap(moving: ReadonlySet<number>, by: number): number | null {
    const { start, end } = this.#plot.view;
    const limit = SNAP_PX * this.#plot.perPx;
    let best: number | null = null;
    for (const m of this.#plot.tracks) {
      if (!moving.has(m.index) || m.tile?.kind !== "edges") continue;
      for (const t of this.#plot.tracks) {
        if (moving.has(t.index) || t.tile?.kind !== "edges") continue;
        const theirs = t.tile.data.ticks;
        for (const e of m.tile.data.ticks) {
          const at = e + by;
          if (at < start || at > end) continue;
          const k = lowerBound(theirs, at);
          for (const j of [k - 1, k]) {
            if (j < 0 || j >= theirs.length) continue;
            const d = theirs[j] - at;
            if (Math.abs(d) <= limit && (best === null || Math.abs(d) < Math.abs(best))) best = d;
          }
        }
      }
    }
    return best;
  }

  /** How far a key moves a grip or a trim: about a pixel, or ten with Shift. */
  #keyStep(shift: boolean): number {
    return Math.max(1, Math.round(this.#plot.perPx)) * (shift ? 10 : 1);
  }

  #placeShaping(): void {
    const l = this.#plot.layout;
    const rows = this.#shaping && l ? l.rows : [];
    const shown = new Set(rows.map((r) => r.track.index).filter((i) => !this.#derived(i)));
    for (const [i, g] of this.#grips) {
      if (shown.has(i)) continue;
      g.remove();
      this.#grips.delete(i);
    }
    for (const [i, pair] of this.#trims) {
      if (shown.has(i)) continue;
      for (const h of pair) h.remove();
      this.#trims.delete(i);
    }
    for (const r of rows) {
      const i = r.track.index;
      if (this.#derived(i)) continue;
      let g = this.#grips.get(i);
      if (!g) {
        g = this.#grip(i);
        this.#grips.set(i, g);
        this.#plotBox.append(g);
      }
      if (!this.#trims.has(i)) {
        const pair: [HTMLElement, HTMLElement] = [this.#trim(i, "start"), this.#trim(i, "end")];
        this.#trims.set(i, pair);
        this.#plotBox.append(...pair);
      }
      g.style.top = `${r.top}px`;
      g.style.height = `${r.height}px`;
      const d = this.#own(i);
      const tb = this.#loaded!.info.timebase;
      const where = d === 0 ? "where it was recorded" : `${formatInterval(Math.abs(d), tb, readingStep(this.#loaded!.info, [i]))} ${d > 0 ? "later" : "earlier"} than recorded`;
      g.setAttribute("aria-label", `Move ${r.track.channel.name}, now ${where}. Left and right move it in time, with Shift further; up and down move its row.`);
    }
    // A group's grip on its head band, the first where the group heads more than one.
    const heads = this.#shaping && l ? l.heads.filter((h, k) => l.heads.findIndex((o) => o.group === h.group) === k) : [];
    for (const [name, g] of this.#heads) {
      if (heads.some((h) => h.group === name)) continue;
      g.remove();
      this.#heads.delete(name);
    }
    for (const h of heads) {
      let g = this.#heads.get(h.group);
      if (!g) {
        g = this.#headGrip(h.group);
        this.#heads.set(h.group, g);
        this.#plotBox.append(g);
      }
      g.style.top = `${h.top}px`;
      g.style.height = `${h.height}px`;
      const d = this.#aligns.get(h.group) ?? 0;
      const info = this.#loaded!.info;
      const mine = [...this.#groupOf].flatMap(([i, n]) => (n === h.group ? [i] : []));
      const where = d === 0 ? "where it was recorded" : `${formatInterval(Math.abs(d), info.timebase, readingStep(info, mine))} ${d > 0 ? "later" : "earlier"} than recorded`;
      g.setAttribute("aria-label", `Move ${h.group}, now ${where}. Left and right move all its channels in time, with Shift further.`);
    }
    // The keys go from grip to grip, and handle to handle, in the rows' order, a group's own grip
    // before its rows'.
    const order = rows.flatMap((r) => {
      const head = heads.find((h) => h.index === r.track.index);
      const own = this.#grips.get(r.track.index);
      return [...(head ? [this.#heads.get(head.group)!] : []), ...(own ? [own, ...this.#trims.get(r.track.index)!] : [])];
    });
    const placed = [...this.#plotBox.querySelectorAll(".grip, .trim")];
    if (order.some((el, k) => placed[k] !== el)) {
      const focused = this.root.activeElement as HTMLElement | null;
      this.#plotBox.append(...order);
      focused?.focus({ preventScroll: true });
    }
    this.#placeTrims();
  }

  /** The trim handles at each row's kept ends, where the view shows them. */
  #placeTrims(): void {
    const l = this.#plot.layout;
    const own = this.#asLoaded?.info;
    if (!this.#shaping || !l || !own) return;
    for (const r of l.rows) {
      const i = r.track.index;
      const pair = this.#trims.get(i);
      if (!pair) continue;
      const d = this.#shaped?.offsets[i] ?? 0;
      const k = this.#keep.get(i);
      const c = own.channels[i];
      const ends = [(k?.from ?? channelStart(c)) + d, (k?.to ?? c.end) + d];
      pair.forEach((h, j) => {
        const x = this.#plot.xAt(ends[j]);
        h.hidden = x < l.plotLeft - 1 || x > l.plotRight + 1;
        h.style.left = `${x}px`;
        h.style.top = `${r.top}px`;
        h.style.height = `${r.height}px`;
        h.setAttribute("aria-valuetext", formatInterval(ends[j], own.timebase, readingStep(own, [i])));
      });
    }
  }

  /** Channel `i`'s grip: a drag sideways moves the channel in time, up or down moves its row. */
  #grip(i: number): HTMLElement {
    const g = Object.assign(document.createElement("div"), { className: "grip", tabIndex: 0 });
    g.setAttribute("part", "grip");
    g.setAttribute("role", "group");
    g.setAttribute("aria-roledescription", "move grip");
    let drag: { id: number; x: number; offset: number } | null = null;
    g.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      if (e.button !== 0 || !this.#shaped) return;
      drag = { id: e.pointerId, x: e.clientX, offset: this.#own(i) };
      g.setPointerCapture(e.pointerId);
      g.classList.add("dragging");
      this.#prober.hide();
    });
    g.addEventListener("pointermove", (e) => {
      e.stopPropagation();
      if (drag?.id !== e.pointerId) return;
      const offset = this.#shaped?.offsets[i];
      const to = Math.round(drag.offset + (e.clientX - drag.x) * this.#plot.perPx);
      // Alt moves it freely; else an edge of it lands on another row's edge within a few pixels.
      this.#moveChannel(i, e.altKey ? to : this.#snapMove(i, to));
      if (this.#shaped?.offsets[i] !== offset) this.#shapeChanged("move", i);
      // Up or down over another row: this one takes its place.
      const y = this.#local(e).y;
      const rows = this.#plot.layout?.rows ?? [];
      const over = rows.findIndex((r) => y >= r.top && y < r.top + r.height);
      if (over >= 0 && rows[over].track.index !== i) {
        this.#reorder(i, over);
        this.#shapeChanged("order", i);
      }
      this.#placeShaping();
    });
    const end = (e: PointerEvent) => {
      e.stopPropagation();
      if (drag?.id !== e.pointerId) return;
      drag = null;
      g.classList.remove("dragging");
    };
    g.addEventListener("pointerup", end);
    g.addEventListener("pointercancel", end);
    g.addEventListener("keydown", (e) => {
      const s = this.#shaped;
      if (!s) return;
      const at = this.#plot.tracks.findIndex((t) => t.index === i);
      let change: "move" | "order";
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        this.#moveChannel(i, this.#own(i) + (e.key === "ArrowLeft" ? -1 : 1) * this.#keyStep(e.shiftKey));
        change = "move";
      } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
        this.#reorder(i, at + (e.key === "ArrowUp" ? -1 : 1));
        change = "order";
      } else return;
      e.preventDefault();
      e.stopPropagation();
      this.#placeShaping();
      this.#shapeChanged(change, i);
    });
    return g;
  }

  /** Group `name`'s grip, on its head band: a drag sideways moves every channel in the group in
   * time, snapping an edge of it onto another group's. */
  #headGrip(name: string): HTMLElement {
    const g = Object.assign(document.createElement("div"), { className: "grip head", tabIndex: 0 });
    g.setAttribute("part", "grip head-grip");
    g.setAttribute("role", "group");
    g.setAttribute("aria-roledescription", "move grip");
    let drag: { id: number; x: number; align: number } | null = null;
    g.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      if (e.button !== 0 || !this.#shaped) return;
      drag = { id: e.pointerId, x: e.clientX, align: this.#aligns.get(name) ?? 0 };
      g.setPointerCapture(e.pointerId);
      g.classList.add("dragging");
      this.#prober.hide();
    });
    g.addEventListener("pointermove", (e) => {
      e.stopPropagation();
      if (drag?.id !== e.pointerId) return;
      const was = this.#aligns.get(name) ?? 0;
      const to = Math.round(drag.align + (e.clientX - drag.x) * this.#plot.perPx);
      // Alt moves it freely; else an edge of it lands on another group's edge within a few pixels.
      this.#moveGroup(name, e.altKey ? to : this.#snapGroup(name, to));
      if ((this.#aligns.get(name) ?? 0) !== was) {
        this.#placeShaping();
        this.#shapeChanged("align", name);
      }
    });
    const end = (e: PointerEvent) => {
      e.stopPropagation();
      if (drag?.id !== e.pointerId) return;
      drag = null;
      g.classList.remove("dragging");
    };
    g.addEventListener("pointerup", end);
    g.addEventListener("pointercancel", end);
    g.addEventListener("keydown", (e) => {
      if (!this.#shaped || (e.key !== "ArrowLeft" && e.key !== "ArrowRight")) return;
      e.preventDefault();
      e.stopPropagation();
      this.#moveGroup(name, (this.#aligns.get(name) ?? 0) + (e.key === "ArrowLeft" ? -1 : 1) * this.#keyStep(e.shiftKey));
      this.#placeShaping();
      this.#shapeChanged("align", name);
    });
    return g;
  }

  /** Where channel `i`'s kept span starts or ends: a drag trims it, snapping to its own edges. */
  #trim(i: number, side: "start" | "end"): HTMLElement {
    const h = Object.assign(document.createElement("div"), { className: "trim", tabIndex: 0 });
    h.setAttribute("part", "trim");
    h.setAttribute("role", "slider");
    h.setAttribute("aria-orientation", "horizontal");
    const name = () => this.#asLoaded?.info.channels[i].name ?? "";
    h.setAttribute("aria-label", `${side === "start" ? "Start" : "End"} of what is kept of ${name()}`);
    /** Sets this end at `tick`, in the channel's own ticks. */
    const set = (tick: number) => {
      const c = this.#asLoaded?.info.channels[i];
      if (!c) return;
      const k = this.#keep.get(i) ?? { from: channelStart(c), to: c.end };
      this.#setKeep(i, side === "start" ? { from: tick, to: k.to } : { from: k.from, to: tick });
      this.#placeTrims();
      this.#shapeChanged("trim", i);
    };
    const ownEnd = () => {
      const c = this.#asLoaded!.info.channels[i];
      const k = this.#keep.get(i);
      return side === "start" ? (k?.from ?? channelStart(c)) : (k?.to ?? c.end);
    };
    let drag: number | null = null;
    h.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      if (e.button !== 0) return;
      drag = e.pointerId;
      h.setPointerCapture(e.pointerId);
      h.classList.add("dragging");
      this.#prober.hide();
    });
    h.addEventListener("pointermove", (e) => {
      e.stopPropagation();
      if (drag !== e.pointerId) return;
      const row = this.#plot.tracks.findIndex((t) => t.index === i);
      set(this.#snapped(this.#local(e).x, row) - (this.#shaped?.offsets[i] ?? 0));
    });
    const end = (e: PointerEvent) => {
      e.stopPropagation();
      if (drag !== e.pointerId) return;
      drag = null;
      h.classList.remove("dragging");
    };
    h.addEventListener("pointerup", end);
    h.addEventListener("pointercancel", end);
    h.addEventListener("keydown", (e) => {
      const c = this.#asLoaded?.info.channels[i];
      if (!c) return;
      if (e.key === "ArrowLeft") set(ownEnd() - this.#keyStep(e.shiftKey));
      else if (e.key === "ArrowRight") set(ownEnd() + this.#keyStep(e.shiftKey));
      else if (e.key === "Home" && side === "start") set(channelStart(c));
      else if (e.key === "End" && side === "end") set(c.end);
      else return;
      e.preventDefault();
      e.stopPropagation();
    });
    return h;
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
      this.#prober.hide();
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

  // Analog scales -------------------------------------------------------------------------------

  #placeScales(): void {
    const l = this.#plot.layout;
    if (!l) return;
    l.rows.forEach((r, i) => {
      let s = this.#scales[i] ?? null;
      const c = r.track.channel;
      if (c.kind !== "analog" || !r.range) {
        s?.remove();
        this.#scales[i] = null;
        return;
      }
      if (!s) {
        s = this.#scale(i);
        this.#scales[i] = s;
        // Under the row handles, which come later.
        this.#plotBox.insertBefore(s, this.#plotBox.querySelector(".hint"));
      }
      s.style.top = `${r.top}px`;
      s.style.height = `${r.height}px`;
      s.style.width = `${l.plotLeft}px`;
      const m = r.range.marks;
      const [topLabel, bottomLabel] = formatMarks(m.top, m.bottom, m.place, c.unit);
      s.setAttribute(
        "aria-label",
        `Scale of ${c.name}, ${bottomLabel} to ${topLabel}. Up and down move it, plus and minus zoom it, Enter fits it to what the view shows, Home shows the whole range.`,
      );
    });
    for (const s of this.#scales.splice(l.rows.length)) s?.remove();
  }

  /**
   * The scale over analog row `row`'s labels. Ctrl or Cmd with the wheel zooms its values about the
   * pointer, a drag moves them, two fingers zoom them, and a double click fits them to what the view
   * shows. Up and down move them, plus and minus zoom, Enter fits and Home shows the whole range.
   */
  #scale(row: number): HTMLElement {
    const s = Object.assign(document.createElement("div"), { className: "scale", tabIndex: 0 });
    s.setAttribute("role", "group");
    s.setAttribute("aria-roledescription", "scale");
    const rowOf = () => this.#plot.layout?.rows[row];
    const pointers = new Map<number, number>();
    let drag: { y: number; range: ValueRange } | null = null;
    let pinch: { distance: number; anchor: number; range: ValueRange } | null = null;
    // The plot's own handlers pan, measure and place cursors: none of that while on a scale.
    s.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      const r = rowOf();
      if (e.button !== 0 || !r?.range) return;
      s.setPointerCapture(e.pointerId);
      pointers.set(e.pointerId, e.clientY);
      this.#prober.hide();
      if (pointers.size === 2) {
        const [a, b] = [...pointers.values()];
        const top = this.#plotBox.getBoundingClientRect().top;
        pinch = { distance: Math.abs(a - b) || 1, anchor: this.#plot.valueAt(r, (a + b) / 2 - top), range: r.range };
        drag = null;
      } else if (pointers.size === 1) {
        drag = { y: e.clientY, range: r.range };
        s.classList.add("dragging");
      }
    });
    s.addEventListener("pointermove", (e) => {
      // A pointer passing over lights the row, through the plot's handlers.
      if (!pointers.has(e.pointerId)) return;
      e.stopPropagation();
      pointers.set(e.pointerId, e.clientY);
      const r = rowOf();
      const lim = this.#limits(row);
      if (!r?.range || !lim) return;
      const px = r.height - 12;
      if (pinch && pointers.size === 2) {
        const [a, b] = [...pointers.values()];
        const span = Math.min(lim.maxSpan, Math.max(lim.minSpan, ((pinch.range.hi - pinch.range.lo) * pinch.distance) / (Math.abs(a - b) || 1)));
        const mid = (a + b) / 2 - this.#plotBox.getBoundingClientRect().top;
        const f = (r.top + r.height - 6 - mid) / px;
        this.#setRange(row, { lo: pinch.anchor - f * span, hi: pinch.anchor - f * span + span });
      } else if (drag) {
        const by = ((e.clientY - drag.y) * (drag.range.hi - drag.range.lo)) / px;
        this.#setRange(row, { lo: drag.range.lo + by, hi: drag.range.hi + by });
      }
    });
    const end = (e: PointerEvent) => {
      if (!pointers.delete(e.pointerId)) return;
      e.stopPropagation();
      // The finger left down after two moves nothing.
      if (pinch) drag = null;
      if (pointers.size < 2) pinch = null;
      if (pointers.size === 0) s.classList.remove("dragging");
    };
    s.addEventListener("pointerup", end);
    s.addEventListener("pointercancel", end);
    s.addEventListener("dblclick", (e) => {
      e.stopPropagation();
      this.#fitScale(row);
    });
    s.addEventListener(
      "wheel",
      (e) => {
        // Without Ctrl or Cmd, the plot's handler moves along the capture or says how to zoom.
        if (!(e.ctrlKey || e.metaKey)) return;
        const r = rowOf();
        const lim = this.#limits(row);
        if (!r?.range || !lim) return;
        e.preventDefault();
        e.stopPropagation();
        const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? r.height : 1;
        const at = this.#plot.valueAt(r, this.#local(e).y);
        this.#setRange(row, zoomRange(r.range, at, Math.exp(e.deltaY * unit * 0.004), lim));
      },
      { passive: false },
    );
    s.addEventListener("keydown", (e) => {
      const r = rowOf();
      const lim = this.#limits(row);
      if (!r?.range || !lim) return;
      const span = r.range.hi - r.range.lo;
      const mid = (r.range.lo + r.range.hi) / 2;
      // Up moves the trace up, as a drag up does: the values shown move down.
      if (e.key === "ArrowUp") this.#setRange(row, moveRange(r.range, -span * (e.shiftKey ? 0.5 : 0.125), lim));
      else if (e.key === "ArrowDown") this.#setRange(row, moveRange(r.range, span * (e.shiftKey ? 0.5 : 0.125), lim));
      else if (e.key === "+" || e.key === "=") this.#setRange(row, zoomRange(r.range, mid, 1 / 1.6, lim));
      else if (e.key === "-" || e.key === "_" || e.key === "−") this.#setRange(row, zoomRange(r.range, mid, 1.6, lim));
      else if (e.key === "Enter") this.#fitScale(row);
      else if (e.key === "Home") this.#setRange(row, null);
      else return;
      e.preventDefault();
      e.stopPropagation();
    });
    return s;
  }

  /** Analog row `row`'s values, where they are zoomed in from its whole range; else null. */
  #zoomedIn(row: number): ValueRange | null {
    const range = this.#plot.layout?.rows[row]?.range;
    const lim = this.#limits(row);
    if (!range || !lim) return null;
    return range.hi - range.lo < (lim.whole.hi - lim.whole.lo) * (1 - 1e-9) ? { lo: range.lo, hi: range.hi } : null;
  }

  #limits(row: number): ScaleLimits | null {
    const c = this.#plot.tracks[row]?.channel;
    return c?.kind === "analog" ? scaleLimits(this.#plot.wholeRange(c), c.resolution) : null;
  }

  /** Gives analog row `row` a range of values, within its limits, or its whole range for null. */
  #setRange(row: number, range: ValueRange | null): void {
    const t = this.#plot.tracks[row];
    const lim = this.#limits(row);
    if (!t || !lim) return;
    if (range === null) this.#plot.ranges.delete(t.index);
    else this.#plot.ranges.set(t.index, clampRange(range, lim));
    // A new range can widen its labels, and with them the names column.
    this.#layout(true);
  }

  /** Fits analog row `row`'s range to the least and greatest values it drew last. */
  #fitScale(row: number): void {
    const t = this.#plot.tracks[row];
    const lim = this.#limits(row);
    const shown = t ? this.#plot.shownRange(t.index) : null;
    const to = lim && shown ? fitRange(shown.lo, shown.hi, lim) : null;
    if (to) this.#setRange(row, to);
  }

  // Window ---------------------------------------------------------------------------------------

  /**
   * The last tick the view goes to: the furthest end of the channels it shows, or the recording's
   * length when it shows every channel. A page that shows a few channels of a longer recording sees
   * them fill the view at Fit, not a sliver of it.
   */
  #extent(): number {
    const info = this.#loaded?.info;
    if (!info) return 0;
    return shownExtent(info, this.#plot.tracks.map((t) => t.index));
  }

  /** Where the view's span starts: 0, or before it where a channel's data does. */
  #start(): number {
    const info = this.#loaded?.info;
    if (!info) return 0;
    return shownStart(info, this.#plot.tracks.map((t) => t.index));
  }

  #clampView(): void {
    const info = this.#loaded?.info;
    if (!info) return;
    const extent = this.#extent();
    const lo = this.#start();
    let { start, end } = this.#plot.view;
    const span = Math.min(extent - lo, Math.max(this.#plot.minSpan, end - start));
    const mid = (start + end) / 2;
    start = Math.min(Math.max(lo, mid - span / 2), extent - span);
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

  /** Opens the view at an annotation, over `span` in ticks if one is given. Resolves once it is
   * drawn. */
  async show(id: string, span?: { start: number; end: number } | null): Promise<void> {
    if (!this.open) this.open = true;
    this.update();
    // The load resolves once its annotations are in, which the view may draw before.
    await this.#loading;
    const loaded = this.#loaded;
    if (!loaded) return;
    const a = loaded.annotations.byId.get(id);
    if (!a) {
      this.fail(new Error(`no annotation ${id}`));
      return;
    }
    this.#plot.focus = a.id;
    if (span) this.#plot.view = { start: span.start, end: span.end };
    else {
      const length = a.end === null ? 0 : a.end - a.start;
      const pad = length > 0 ? length * 0.25 : Math.max(this.#plot.minSpan * 10, loaded.info.length / 2000);
      this.#plot.view = { start: a.start - pad, end: (a.end ?? a.start) + pad };
    }
    if (a.kind !== null && this.#kind.value !== a.kind) {
      this.#kind.value = a.kind;
    }
    this.#stepIndex = this.#ofKind().findIndex((b) => b.id === a.id);
    this.#renderSteps();
    this.#viewChanged();
    await this.#plot.whenSettled();
  }

  /** The focus goes to the plot, where the keys work, or to the Open button while the view is closed. */
  override focus(options?: FocusOptions): void {
    (this.open ? this.#plotBox : this.#toggle).focus(options);
  }

  /** Opens the view at an annotation, over `span` if one is given, brings it into sight and puts the
   * focus on its plot: what a reference, an inset or a chart's point does when the reader asks for
   * the recording. */
  async reveal(id: string, span?: { start: number; end: number } | null): Promise<void> {
    const shown = this.show(id, span);
    this.scrollIntoView({ block: "nearest", behavior: reducedMotion() ? "auto" : "smooth" });
    await shown;
    this.#plotBox.focus({ preventScroll: true });
  }

  /** The whole capture, and each analog row's whole range. */
  #fit(): void {
    if (!this.#loaded) return;
    if (this.#plot.ranges.size > 0) {
      this.#plot.ranges.clear();
      this.#layout(true);
    }
    this.setWindow(this.#start(), this.#extent());
  }

  /** Zooms by `factor` (above 1 zooms out) about x, or the middle. */
  #zoomBy(factor: number, x: number | null): void {
    if (!this.#plot.layout) return;
    const { start, end } = this.#plot.view;
    const at = x === null ? (start + end) / 2 : this.#plot.tickAt(x);
    const span = Math.max(this.#plot.minSpan, (end - start) * factor);
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
    this.#prober.probe();
    if (this.#windowEventQueued) return;
    this.#windowEventQueued = true;
    requestAnimationFrame(() => {
      this.#windowEventQueued = false;
      this.emit("tickwave-window", this.window);
    });
  }

  #drawn(): void {
    if (this.#readyFired || !this.#plot.settled || !this.#annotated) return;
    this.#readyFired = true;
    queueMicrotask(() => this.emit("tickwave-ready", { height: this.offsetHeight }));
  }

  /** Resolves after the next frame drawn with every row's data and the annotations. */
  async settled(): Promise<void> {
    await this.#loading;
    await this.#plot.whenSettled();
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
      // A finger moving up or down scrolls the page, as the plot's touch-action leaves it to.
      const values = hit.area === "row" && e.pointerType !== "touch" ? this.#zoomedIn(hit.row) : null;
      this.#drag = {
        id: e.pointerId,
        x: p.x,
        y: p.y,
        area: hit.area,
        row: hit.area === "row" ? hit.row : -1,
        view: { ...this.#plot.view },
        values,
        moved: false,
      };
    });
    box.addEventListener("pointermove", (e) => {
      const p = this.#local(e);
      if (e.pointerType === "mouse" || e.pointerType === "pen") this.#prober.pointer = p;
      if (this.#pointers.has(e.pointerId)) this.#pointers.set(e.pointerId, p);
      const pinch = this.#pinch;
      if (pinch && this.#pointers.size === 2) {
        this.#prober.hide();
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
        this.#prober.hide();
        if (d.area === "axis") {
          const l = this.#plot.layout!;
          const inside = (x: number) => Math.max(l.plotLeft, Math.min(l.plotRight, x));
          this.#plot.selection = { start: this.#plot.tickAt(inside(d.x)), end: this.#plot.tickAt(inside(p.x)) };
          this.#plot.invalidate();
        } else {
          box.classList.add("panning");
          const per = (d.view.end - d.view.start) / (this.#plot.layout!.plotRight - this.#plot.layout!.plotLeft);
          this.#plot.view = { start: d.view.start - (p.x - d.x) * per, end: d.view.end - (p.x - d.x) * per };
          const r = this.#plot.layout!.rows[d.row];
          if (d.values && r) {
            const by = ((p.y - d.y) * (d.values.hi - d.values.lo)) / (r.height - 12);
            this.#setRange(d.row, { lo: d.values.lo + by, hi: d.values.hi + by });
          }
          this.#viewChanged();
        }
        return;
      }
      this.#hover(p.x, p.y);
      this.#prober.probe();
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
        this.#prober.probe();
        return;
      }
      const hit = this.#plot.hit(p.x, p.y);
      const l = this.#plot.layout!;
      if (hit.area === "lane" && hit.note) {
        void this.show(hit.note.id);
        this.emit("tickwave-annotation", { id: hit.note.id });
      }
      else if (hit.area === "row" && p.x >= l.plotLeft && p.x <= l.plotRight) this.#placeCursor(p.x, hit.row);
    };
    box.addEventListener("pointerup", end);
    box.addEventListener("pointercancel", end);
    box.addEventListener("pointerleave", () => {
      this.#prober.pointer = null;
      this.#prober.hide();
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
        this.#hint(e.target instanceof Element && e.target.classList.contains("scale") ? "the scale" : null);
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
        case "ArrowUp":
        case "+":
        case "=":
          this.#zoomBy(1 / 1.6, null);
          break;
        case "ArrowDown":
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
          this.#setCursors(null, null);
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

  /** Says how to zoom: the capture, or `what`. */
  #hint(what: string | null = null): void {
    const hint = this.root.querySelector(".hint") as HTMLElement;
    hint.textContent = `${zoomKey()} + scroll to zoom${what ? ` ${what}` : ""}`;
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
    this.#placeCursorAt(this.#snapped(x, row));
  }

  /** The tick at `x`, or the nearest edge of row `row` within a few pixels of it. */
  #snapped(x: number, row: number): number {
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
    return tick;
  }

  /** Places cursor A, or B after it, at `tick`. */
  #placeCursorAt(tick: number): void {
    if (this.#nextCursor === "a") {
      this.#setCursors(tick, null);
      this.#nextCursor = "b";
    } else {
      this.#setCursors(this.#plot.cursors.a, tick);
      this.#nextCursor = "a";
    }
    this.#plot.invalidate();
    void this.#renderReadout();
  }

  /** Moves the cursors, and tells the page with `tickwave-cursor` when they change: each cursor's
   * tick, and its time at the recording's resolution; null where one is not placed. */
  #setCursors(a: number | null, b: number | null): void {
    const was = this.#plot.cursors;
    this.#plot.cursors = { a, b };
    if (was.a === a && was.b === b) return;
    const info = this.#loaded?.info;
    // A cursor is tied to no channel: it reads at the finest source's tick.
    const time = (t: number | null) => (t === null || !info ? null : formatInterval(t, info.timebase, readingStep(info)));
    this.emit("tickwave-cursor", { a, b, aTime: time(a), bTime: time(b) });
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

  // The label at the pointer ----------------------------------------------------------------------

  readonly #prober: Prober;

  #probeBox(): HTMLElement {
    return this.root.querySelector(".reading") as HTMLElement;
  }

  /**
   * What the prober reads: the label, under the pulse it measures or the pointer, with the figures
   * that do not sit on the pulse's spans.
   */
  #shown(r: Probed | null): void {
    const box = this.#probeBox();
    const p = this.#prober.pointer;
    const l = this.#plot.layout;
    if (!r || !p || !l) {
      box.hidden = true;
      return;
    }
    if (r.kind === "text") {
      this.#showProbe(r.lines, p, l.rows[r.row]);
      return;
    }
    const { figures: f, mark, on } = r;
    const lines: string[] = [];
    if (!on.width) lines.push(`${f.level} ${f.width}`);
    if (f.period) {
      if (!on.period) lines.push(`period ${f.period}`);
      lines.push(`duty ${f.duty}`, f.frequency ?? "–");
    }
    if (f.inverseWidth) lines.push(`1/width ${f.inverseWidth}`);
    const span = mark && { x0: this.#plot.xAt(mark.start), x1: this.#plot.xAt(mark.next ?? mark.end) };
    this.#showProbe(lines, p, l.rows[r.row], span);
  }

  /** The label, away from the row it reads, under the pulse it measures (`span`) or the pointer. */
  #showProbe(lines: readonly string[], p: { x: number; y: number }, row: { top: number; height: number }, span: { x0: number; x1: number } | null = null): void {
    const box = this.#probeBox();
    box.replaceChildren(
      ...lines.map((text, i) => Object.assign(document.createElement("div"), { textContent: text, className: i > 0 ? "more" : "" })),
    );
    box.hidden = false;
    const l = this.#plot.layout!;
    const w = box.offsetWidth;
    const h = box.offsetHeight;
    // Under the pulse it measures, or the pointer, inside the plot's width.
    const middle = span ? (span.x0 + span.x1) / 2 : p.x;
    const x = Math.max(l.plotLeft, Math.min(l.plotRight - w, middle - w / 2));
    // On the rows' edge away from the row read, so the label never covers the wave there: flush with
    // that edge, or just past the row where the label is taller than the room, running past the
    // plot's edge in a short plot.
    const last = l.rows[l.rows.length - 1];
    const [top, bottom] = [l.rows[0].top, last.top + last.height];
    const below = row.top + row.height / 2 <= (top + bottom) / 2;
    const y = below ? Math.max(row.top + row.height + LABEL_GAP, bottom - h) : Math.min(row.top - LABEL_GAP - h, top);
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
    // Cursors and the selection are tied to no channel: the finest source's tick.
    const free = readingStep(l.info);
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
      const parts = [item("A", formatInterval(a, tb, free))];
      if (b !== null) {
        parts.push(item("B", formatInterval(b, tb, free)));
        const dt = b - a;
        const rate = formatRate(1, Math.abs(dt), tb, 1, free);
        parts.push(item("Δt", formatInterval(dt, tb, free) + (rate ? ` (${rate})` : "")));
      }
      line(...parts);
      // Each analog channel's value at the cursors, to the digit of one code step.
      for (const t of this.#plot.tracks) {
        const c = t.channel;
        if (c.kind !== "analog") continue;
        const at = async (tick: number) => {
          // A channel whose breaks are in pages is placed by those an answer at the tick carries.
          const cc = c.paged ? spanned(c, (await l.source.samples(t.index, tick, tick + 1)).listed) : c;
          if (cc.kind !== "analog") return null;
          const i = sampleAtOrBefore(cc, tick);
          if (i === null) return null;
          const from = channelSampleTick(cc, i);
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
        const figure = (v: number | null) => (v === null || !Number.isFinite(v) ? "–" : formatValue(v, c.resolution, c.unit, c.significant));
        parts.push(item("A", figure(values[0])));
        if (b !== null) {
          parts.push(item("B", figure(values[1])));
          if (values[0] !== null && values[1] !== null) parts.push(item("Δ", figure(values[1] - values[0])));
        }
        line(...parts);
      }
      // Each digital channel's pulse at cursor A, as the label at the pointer gives it.
      for (const t of this.#plot.tracks) {
        let c = t.channel;
        if (c.kind !== "digital" || (!c.paged && !inChannel(c, a))) continue;
        let pulse: Pulse;
        try {
          const e = await l.source.edges(t.index, a, a + 1);
          // A channel whose gaps are in pages knows them around the cursor from the answer.
          c = spanned(c, e.listed);
          if (!inChannel(c, a)) continue;
          pulse = await completePulse(l.source, t.index, pulseFromEdges(e, a, a + 1, knownSpan(c, a), a), a, knownSpan(c, a));
        } catch (e) {
          failed(e);
          return;
        }
        if (seq !== this.#readoutSeq) return;
        const f = pulseFigures(pulse, tb, knownSpan(c, a), readingStep(l.info, [t.index]));
        const parts: (string | Node)[] = [c.name, item(f.level, f.width)];
        if (f.inverseWidth) parts.push(item("1/width", f.inverseWidth));
        if (f.period) parts.push(item("period", f.period), fig(f.frequency ?? "–"), item("duty", f.duty ?? "–"));
        line(...parts);
      }
    }
    const sel = this.#plot.selection;
    let detail: Record<string, unknown> | null = null;
    if (sel) {
      line(item("Selection", `${formatInterval(sel.start, tb, free)} to ${formatInterval(sel.end, tb, free)}`), item("Δt", formatInterval(sel.end - sel.start, tb, free)));
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
          const step = readingStep(l.info, [digital[chosen].index]);
          const rate = formatClockRate(stats, tb, step);
          const period = formatClockPeriod(stats, tb, step);
          periods.append(
            item("count", formatCount(stats.count)),
            item("min", formatInterval(stats.min, tb, step)),
            item("median", formatInterval(stats.median, tb, step)),
            item("max", formatInterval(stats.max, tb, step)),
            item("clock", rate),
            clockNote(stats),
          );
          periodDetail = { channel: digital[chosen].channel.name, ...stats, rate, period };
        } else periods.append("fewer than two rising edges");
        detail = { start: sel.start, end: sel.end, duration: formatInterval(sel.end - sel.start, tb, free), edges: edgeCounts, periods: periodDetail };
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

/** What `summary` and the `tickwave-summary` event give. */
export interface Summary {
  /** The recording's own title, and its facts as one line: "524 ms; 2 channels: SWCLK, SWDIO; 4.7 KiB". */
  readonly title: string;
  readonly facts: string;
  /** Whether the text the page slotted, where it slotted any, matches them. */
  readonly matches: boolean;
}

/** A slot's assigned text with its whitespace collapsed, or null where the page slotted nothing. */
function slotted(slot: HTMLSlotElement): string | null {
  const nodes = slot.assignedNodes({ flatten: true });
  return nodes.length === 0 ? null : collapse(nodes.map((n) => n.textContent ?? "").join(""));
}

function collapse(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

function part(name: string, text: string): HTMLSpanElement {
  const span = document.createElement("span");
  span.setAttribute("part", name);
  span.textContent = text;
  return span;
}
