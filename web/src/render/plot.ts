// The canvas renderer: an annotation lane, one row a channel, and a time axis, drawn at the device
// pixel ratio. It draws whatever data has arrived and asks for more; a fetch that completes
// schedules another frame.

import { covers, laneAnnotations, type Annotation, type Resolved } from "../annotations.ts";
import { rgba, type Palette } from "../palette.ts";
import { formatMarks, niceStep } from "../readouts.ts";
import { rangeMarks, type ValueRange } from "./scale.ts";
import {
  axisPeriod,
  channelExtent,
  channelStart,
  endLosses,
  knownSpans,
  lostSpans,
  named,
  rebased,
  sampleTicks,
  spanned,
  type AnalogChannel,
  type Channel,
  type RecordingInfo,
  type Source,
} from "../source.ts";
import { timeAxis } from "./axis.ts";
import {
  analogFromOverview,
  analogFromSamples,
  digitalFromEdges,
  digitalFromLevels,
  lineSamples,
  type AnalogColumns,
  type DigitalColumns,
  type Grid,
} from "./columns.ts";
import { layoutNotes, type NoteIn, type NoteOut } from "./notes.ts";
import { looks } from "./shade.ts";
import { digitalTrace, type Fills, type Pen } from "./trace.ts";
import { grain, Track, type Layer, type Tile } from "./track.ts";

/** A piece of a row's trace: its line, stroked a path at a time, its fill, and a digital row's
 * fills (trace.ts). */
interface Shapes {
  readonly lines: readonly Line[];
  readonly fill: Path2D;
  readonly fills?: Strips;
}

/** A path of a row's line, and the device-pixel columns its stroke can paint, `x0` to `x1`. */
interface Line {
  readonly path: Path2D;
  x0: number;
  x1: number;
}

/**
 * A digital row's line as a pen, in a path for each `width` columns from `left`, so a stretch of the
 * row strokes only the paths it overlaps. A trace's lines are level or upright (trace.ts): a level
 * one goes on in the next path from each boundary it crosses, butt ends meeting at a whole pixel.
 * One that starts on a boundary carries its path past it, so the join before it stays in one path.
 * A move past the path's columns starts another.
 */
class Lines implements Pen {
  readonly lines: Line[] = [];
  #line: Line | null = null;
  /** Where the current path's columns end, and the pen's place. */
  #end = 0;
  #x = 0;
  #y = 0;
  readonly left: number;
  readonly width: number;
  /** How far past its points a path's stroke paints. */
  readonly reach: number;

  constructor(left: number, width: number, reach: number) {
    [this.left, this.width, this.reach] = [left, width, reach];
  }

  #start(x: number, y: number): void {
    this.#line = { path: new Path2D(), x0: x - this.reach, x1: x + this.reach };
    this.lines.push(this.#line);
    this.#line.path.moveTo(x, y);
    this.#end = this.left + (Math.floor((x - this.left) / this.width) + 1) * this.width;
    [this.#x, this.#y] = [x, y];
  }

  moveTo(x: number, y: number): void {
    if (!this.#line || x >= this.#end || x < this.#end - this.width) return this.#start(x, y);
    this.#line.path.moveTo(x, y);
    [this.#x, this.#y] = [x, y];
  }

  lineTo(x: number, y: number): void {
    if (y === this.#y) {
      while (this.#end < x) {
        if (this.#end === this.#x) {
          this.#end += this.width;
          continue;
        }
        this.#line!.path.lineTo(this.#end, y);
        this.#line!.x1 = Math.max(this.#line!.x1, this.#end + this.reach);
        this.#start(this.#end, y);
      }
    }
    const l = this.#line!;
    l.path.lineTo(x, y);
    l.x0 = Math.min(l.x0, x - this.reach);
    l.x1 = Math.max(l.x1, x + this.reach);
    [this.#x, this.#y] = [x, y];
  }
}

/** A digital row's fills: each layer's alpha, a column a device pixel from `left`, and the
 * layer's top and depth in device pixels, the band's and each rail's. */
interface Strips {
  readonly fills: Fills;
  readonly left: number;
  readonly layers: readonly (readonly [number, number])[];
}

export interface PlotConfig {
  readonly axis: boolean;
  /** Sub-lanes in the annotation lane; 0 for none. */
  readonly lanes: number;
  readonly rowHeight: number;
  readonly analogHeight: number;
  /** Draw the boundaries of every note across the rows, as the inset does. */
  readonly noteRules: boolean;
  /** The size of channel names and notes, in CSS pixels. */
  readonly labelPx: number;
}

export interface Row {
  readonly track: Track;
  readonly top: number;
  readonly height: number;
  /** For an analog row, the values at its bottom and top, and the two values its scale labels. */
  readonly range?: RowRange;
}

export interface RowRange extends ValueRange {
  /** The labelled values, each a multiple of 10^place. */
  readonly marks: { readonly top: number; readonly bottom: number; readonly place: number };
}

/** A band above the first row of a group of channels, such as one input of a merge, with its name. */
export interface Head {
  readonly group: string;
  /** The channel index of the row it heads. */
  readonly index: number;
  readonly top: number;
  readonly height: number;
}

export interface Layout {
  readonly width: number;
  readonly height: number;
  readonly plotLeft: number;
  readonly plotRight: number;
  readonly laneTop: number;
  readonly laneHeight: number;
  readonly rows: readonly Row[];
  readonly heads: readonly Head[];
  readonly axisTop: number;
}

const AXIS_PX = 11;
const LANE_ROW = 18;
const AXIS_HEIGHT = 24;
/** A group's head band: its name at the labels' size, with room above and below. */
const HEAD_PX = 22;
/** The selection's bar along the top of the axis band, clear of the labels below it. */
const SELECTION_BAR = 4;
const RIGHT_PAD = 10;
/** The opacity of an analog band a coarse layer cannot place: the trace's colour, faded. */
const UNRESOLVED_ALPHA = 0.35;
/** The segments in each path of a line through samples: Firefox strokes a long path of steep
 * segments far slower than the same segments a few hundred at a time. */
const LINE_SEGMENTS = 256;
/** The device-pixel columns of each path of a digital row's line (`Lines`). */
const LINE_COLUMNS = 256;
/** What the row of a channel this version can't read says. */
export const UNAVAILABLE = "unavailable to this version";
/** Paint roles that draw heavier than a trace, by `HEAVIER` CSS pixels, so their lines differ from
 * the others' by more than colour: the target's drive. */
export const HEAVY_ROLES: ReadonlySet<string> = new Set(["target"]);
const HEAVIER = 1;
/** The fills strips a plot keeps: more than the pieces and colours a row's stretches take turns
 * at. */
const STRIPS = 8;
/** A zoom step's ease, in milliseconds, in a view and an inset. */
export const EASE_MS = 150;
/** How long a read may run before the element says it is still loading. */
const STALL_MS = 5000;

/** A failed read as the reader meets it: data that did not load. The name stays, for a page to tell the cause. */
function notLoaded(e: unknown): Error {
  const out = named(e instanceof Error ? e.name : "Error", `Data did not load: ${e instanceof Error ? e.message : String(e)}`);
  out.cause = e;
  return out;
}

/** Where channel `c` lost data, as `layers` draw it: a digital channel's gaps, an analog channel's
 * lost samples. A channel whose list is in pages has each layer's, inside the layer's span, and
 * the samples lost before its first from its header. */
export function lostIn(c: Channel, layers: readonly Layer[]): readonly { from: number; to: number }[] {
  const of = (cc: Channel): readonly { from: number; to: number }[] => (cc.kind === "digital" ? (cc.gaps ?? []) : cc.kind === "analog" ? lostSpans(cc) : []);
  if (c.kind === "unavailable" || !c.paged) return of(c);
  const out: { from: number; to: number }[] = c.kind === "analog" ? endLosses(c) : [];
  // The loss before the first is the header's, already in: no layer pales it again.
  const rest: Channel = c.kind === "analog" ? { ...c, lostBefore: undefined } : c;
  for (const { tile, start, end } of layers) {
    for (const g of of(spanned(rest, tile.data.listed))) {
      const from = Math.max(g.from, start);
      const to = Math.min(g.to, end);
      if (to > from) out.push({ from, to });
    }
  }
  return out;
}

/**
 * A digital pulse measured under the pointer, drawn inside its row: a span along the pulse's level
 * between its edges, and one along the other level over its period. The trace's own edges end both.
 */
export interface PulseMark {
  readonly row: number;
  readonly level: 0 | 1;
  readonly start: number;
  readonly end: number;
  /** The edge that closes the period; null where there is none. */
  readonly next: number | null;
  /** The figures for the two spans. Each sits on its span where `markFits` says it does. */
  readonly width: string;
  readonly period: string | null;
}

/** The closest zoom: this many CSS pixels a tick. */
const MAX_PX_PER_TICK = 40;

/** A span's figure: its box's height, and the room it keeps from the span's ends. */
const FIGURE_BOX = 16;
const FIGURE_PAD = 4;
const ARROW = 5;
/** A figure's box lets its span show faintly through. */
const FIGURE_ALPHA = 0.8;
/** Between the trace's levels, the room two figures and their spans need. */
const FIGURES_ROOM = 30;

export type Hit =
  | { readonly area: "lane"; readonly note: NoteOut | null }
  | { readonly area: "row"; readonly row: number }
  | { readonly area: "axis" }
  | { readonly area: "none" };

export class Plot {
  readonly canvas: HTMLCanvasElement;
  readonly #ctx: CanvasRenderingContext2D;
  config: PlotConfig;
  info: RecordingInfo | null = null;
  tracks: Track[] = [];
  annotations: Resolved | null = null;
  /** The annotation lane shows even with nothing in it, as while a page edits the annotations. */
  keepLane = false;
  /** Said in a kept lane where no annotation falls on a shown channel, anywhere in the recording.
   * Null while the annotations are read, or where the sidecar failed and the notice speaks. */
  emptyLane: string | null = null;
  /** Whether an annotation falls on a shown channel, for these annotations and these channels. */
  #onShown: { annotations: Resolved | null; channels: string; on: boolean } | null = null;
  /** CSS pixels kept at the names column's left for the view's move grips, while it shapes. */
  grip = 0;
  /** Rows' kept spans, by channel index, in the plot's ticks: the trace outside one fades, as the
   * part a trim leaves out. */
  kept = new Map<number, { from: number; to: number }>();
  /** Each channel's group, by channel index: a row whose group differs from the row above's gets a
   * head band with the group's name. Empty: no heads. */
  groups = new Map<number, string>();
  #palette: Palette | null = null;
  /** Where a row's fills are put before they are drawn (`#drawFills`): a few strips, each holding
   * one piece's fills in one colour, the one used last first. */
  #strips: { ctx: CanvasRenderingContext2D; holds: Fills | null; colour: string }[] = [];
  layout: Layout | null = null;
  #view = { start: 0, end: 1 };
  /** A zoom step under way: the window drawn goes from `from` to `to` over `ms` from `t0`. */
  #ease: { from: { start: number; end: number }; to: { start: number; end: number }; t0: number; ms: number } | null = null;
  cursors: { a: number | null; b: number | null } = { a: null, b: null };
  selection: { start: number; end: number } | null = null;
  hoverRow: number | null = null;
  hoverNote: string | null = null;
  pulseMark: PulseMark | null = null;
  /** Row heights the reader set, by channel index; the others take the config's. */
  heights = new Map<number, number>();
  /** Value ranges the reader set for analog rows, by channel index; the others show the whole. */
  ranges = new Map<number, ValueRange>();
  /** The values each analog row drew last, by channel index. */
  readonly #shown = new Map<number, ValueRange>();
  /** The annotation the element is about, drawn emphasised. */
  focus: string | null = null;
  /** When set, the lane draws only these annotations. */
  laneFilter: ReadonlySet<string> | null = null;
  /** Traces and notes are drawn only left of this x (CSS px); null draws everything. */
  reveal: number | null = null;
  notes: NoteOut[] = [];
  onDraw: (() => void) | null = null;
  onError: ((e: unknown) => void) | null = null;
  /** A problem `onError` reported that has gone: every track has read since. */
  onRecovered: ((message: string) => void) | null = null;
  /** The failed reads reported, until every track reads again. */
  #notLoaded = new Set<string>();
  /** Called with STALL_MS once a read has run that long, and with null once no read is running. */
  onStall: ((waitedMs: number | null) => void) | null = null;
  #stallTimer: ReturnType<typeof setTimeout> | undefined;
  #stalled = false;
  #frame = 0;
  #dpr = 1;
  #settledWaiters: (() => void)[] = [];
  /** Text widths by font and text. A frame measures in several fonts, so one cache holds them all;
   * a new palette, which a face or a web font arriving brings, empties it. */
  #measureCache = new Map<string, number>();

  constructor(canvas: HTMLCanvasElement, config: PlotConfig) {
    this.canvas = canvas;
    this.#ctx = canvas.getContext("2d")!;
    this.config = config;
  }

  setRecording(source: Source, info: RecordingInfo, channels: readonly number[]): void {
    this.info = info;
    // The rows this replaces may still be reading. What they find, or fail to, is not about these.
    const tracks: Track[] = channels.map(
      (i) =>
        new Track(
          source,
          i,
          info.channels[i],
          () => {
            if (this.tracks !== tracks) return;
            this.#recovered();
            this.invalidate();
          },
          (e) => {
            if (this.tracks !== tracks) return;
            this.#watchStall();
            const error = notLoaded(e);
            this.#notLoaded.add(error.message);
            this.onError?.(error);
          },
        ),
    );
    this.tracks = tracks;
    this.layout = null;
  }

  /** Once no track's last read failed, the failures reported are over. */
  #recovered(): void {
    if (this.#notLoaded.size === 0 || this.tracks.some((t) => t.failed)) return;
    for (const message of this.#notLoaded) this.onRecovered?.(message);
    this.#notLoaded.clear();
  }

  // Layout -------------------------------------------------------------------------------------

  #font(px: number, weight = 400, family = this.palette?.fontText ?? "sans-serif"): string {
    return `${weight} ${px}px ${family}`;
  }

  get palette(): Palette | null {
    return this.#palette;
  }

  set palette(p: Palette | null) {
    this.#palette = p;
    this.#measureCache.clear();
  }

  measure(text: string, font: string): number {
    const key = `${font}\n${text}`;
    let w = this.#measureCache.get(key);
    if (w === undefined) {
      // Eliding probes many prefixes of each label: bounded, so a long session does not grow it.
      if (this.#measureCache.size > 8192) this.#measureCache.clear();
      this.#ctx.font = font;
      w = this.#ctx.measureText(text).width;
      this.#measureCache.set(key, w);
    }
    return w;
  }

  /** Annotations the lane draws. */
  laneNotes(): Annotation[] {
    const r = this.annotations;
    if (!r) return [];
    return laneAnnotations(r, new Set(this.tracks.map((t) => t.channel.name)), this.laneFilter);
  }

  /** What the lane says for itself: `emptyLane` where no annotation, drawn in the lane or painting
   * a row, falls on a shown channel; else nothing. */
  laneSays(): string | null {
    const say = this.emptyLane;
    const l = this.layout;
    if (say === null || !l || l.laneHeight === 0) return null;
    // Asked at every draw; the list is walked again only for other annotations or other channels.
    const names = this.tracks.map((t) => t.channel.name);
    const key = names.join("\n");
    let m = this.#onShown;
    if (!m || m.annotations !== this.annotations || m.channels !== key) {
      const shown = new Set(names);
      const on = (this.annotations?.byTime ?? []).some((a) => a.channels === null || a.channels.some((c) => shown.has(c)));
      m = this.#onShown = { annotations: this.annotations, channels: key, on };
    }
    return m.on ? null : say;
  }

  /** An analog channel's whole range: its least and greatest values, out to round steps. */
  wholeRange(c: AnalogChannel): { lo: number; hi: number; place: number } {
    const lo0 = Number.isFinite(c.min) ? c.min : 0;
    const hi0 = Number.isFinite(c.max) ? Math.max(c.max, lo0) : lo0;
    const spread = hi0 > lo0 ? hi0 - lo0 : Math.max(c.resolution ?? 0, Math.abs(hi0) * 1e-3, 1e-12);
    const { step, place } = niceStep(spread / 4);
    return { lo: Math.floor(lo0 / step) * step, hi: Math.max(Math.ceil(hi0 / step) * step, Math.floor(lo0 / step) * step + step), place };
  }

  #rowRange(t: Track): RowRange | undefined {
    const c = t.channel;
    if (c.kind !== "analog") return undefined;
    const set = this.ranges.get(t.index);
    if (set) return { lo: set.lo, hi: set.hi, marks: rangeMarks(set) };
    const whole = this.wholeRange(c);
    return { lo: whole.lo, hi: whole.hi, marks: { top: whole.hi, bottom: whole.lo, place: whole.place } };
  }

  /** The least and greatest values row `index`'s channel drew last, or null if it drew none. */
  shownRange(index: number): ValueRange | null {
    return this.#shown.get(index) ?? null;
  }

  /** Where value `v` sits in row `r`, in CSS pixels: its range spans the row less 6 px each end. */
  valueY(r: Row, v: number): number {
    const range = r.range!;
    const top = r.top + 6;
    const bottom = r.top + r.height - 6;
    return bottom - ((v - range.lo) / (range.hi - range.lo)) * (bottom - top);
  }

  /** The value at `y` in row `r`, the inverse of valueY. */
  valueAt(r: Row, y: number): number {
    const range = r.range!;
    const top = r.top + 6;
    const bottom = r.top + r.height - 6;
    return range.lo + ((bottom - y) / (bottom - top)) * (range.hi - range.lo);
  }

  defaultHeight(t: Track): number {
    return t.channel.kind === "analog" ? this.config.analogHeight : this.config.rowHeight;
  }

  /** Whether the row at position `i` starts a group, and so has a head band above it. */
  #headed(i: number): boolean {
    const g = this.groups.get(this.tracks[i].index);
    return g !== undefined && (i === 0 || this.groups.get(this.tracks[i - 1].index) !== g);
  }

  /** The space above the row at position `i`: a head band, or an analog row's room for its top label. */
  #above(i: number): number {
    if (this.#headed(i)) return HEAD_PX;
    return i > 0 && this.tracks[i].channel.kind === "analog" ? 8 : 0;
  }

  /** The lane, the rows at their own heights and the axis: what the plot stacks, with no height given. */
  #stack(): { laneHeight: number; top: number; natural: number[]; fixed: number } {
    const laneHeight = this.config.lanes > 0 && (this.keepLane || this.laneNotes().length > 0) ? this.config.lanes * LANE_ROW + 4 : 0;
    const top = 2 + laneHeight + (laneHeight > 0 ? 2 : 4);
    const natural = this.tracks.map((t) => this.heights.get(t.index) ?? this.defaultHeight(t));
    const gaps = this.tracks.reduce((sum, _, i) => sum + this.#above(i), 0);
    const fixed = top + gaps + natural.reduce((a, b) => a + b, 0) + (this.config.axis ? AXIS_HEIGHT + 2 : 4);
    return { laneHeight, top, natural, fixed };
  }

  /** Lays the plot out at `width` CSS pixels; `height` above the natural one goes to the rows. */
  resize(width: number, height: number | null): Layout {
    const font = this.#font(this.config.labelPx, 500);
    let gutter = 0;
    const ranges = this.tracks.map((t) => this.#rowRange(t));
    this.tracks.forEach((t, i) => {
      gutter = Math.max(gutter, this.measure(t.channel.name, font));
      const m = ranges[i]?.marks;
      const c = t.channel;
      if (m && c.kind === "analog") {
        const axisFont = this.#font(AXIS_PX);
        const [top, bottom] = formatMarks(m.top, m.bottom, m.place, c.unit);
        gutter = Math.max(gutter, this.measure(top, axisFont), this.measure(bottom, axisFont));
      }
    });
    const plotLeft = Math.ceil(Math.min(Math.max(gutter + 14, 40), Math.max(40, width * 0.3))) + this.grip;
    const laneTop = 2;
    const { laneHeight, top: rowsTop, natural, fixed } = this.#stack();
    let top = rowsTop;
    const extra = height !== null ? Math.max(0, height - fixed) : 0;
    const analogs = this.tracks.filter((t) => t.channel.kind === "analog").length;
    const heads: Head[] = [];
    const rows: Row[] = this.tracks.map((track, i) => {
      // An analog row's scale labels sit at its top and bottom edges; keep them off the row above.
      // A head band does that too.
      const above = this.#above(i);
      if (this.#headed(i)) heads.push({ group: this.groups.get(track.index)!, index: track.index, top, height: above });
      top += above;
      let h = natural[i];
      if (extra > 0) {
        h += analogs > 0 ? (track.channel.kind === "analog" ? extra / analogs : 0) : Math.min(this.config.rowHeight, extra / this.tracks.length);
      }
      const row: Row = { track, top, height: h, ...(ranges[i] && { range: ranges[i] }) };
      top += h;
      return row;
    });
    const axisTop = top + 2;
    this.layout = {
      width,
      height: Math.ceil(this.config.axis ? axisTop + AXIS_HEIGHT : top + 4),
      plotLeft,
      plotRight: Math.max(plotLeft + 1, width - RIGHT_PAD),
      laneTop,
      laneHeight,
      rows,
      heads,
      axisTop,
    };
    this.#dpr = Math.max(1, globalThis.devicePixelRatio || 1);
    // Setting a canvas's size clears it, even to the size it has, and the page can paint before the
    // next frame draws: a row dragged taller would flash blank. So keep a bitmap that still fits,
    // and draw a new one at once.
    const w = Math.round(width * this.#dpr);
    const h = Math.round(this.layout.height * this.#dpr);
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${this.layout.height}px`;
    if (w !== this.canvas.width || h !== this.canvas.height) {
      this.canvas.width = w;
      this.canvas.height = h;
      this.drawNow();
    }
    this.invalidate();
    return this.layout;
  }

  // Coordinates --------------------------------------------------------------------------------

  /** The narrowest span a reader can zoom to, in ticks: MAX_PX_PER_TICK pixels a tick. */
  get minSpan(): number {
    const l = this.layout;
    return l ? Math.max(2, (l.plotRight - l.plotLeft) / MAX_PX_PER_TICK) : 2;
  }

  /** Ticks a CSS pixel in the plot. */
  get perPx(): number {
    const l = this.layout;
    return l ? (this.view.end - this.view.start) / (l.plotRight - l.plotLeft) : 1;
  }

  xAt(tick: number): number {
    const l = this.layout!;
    return l.plotLeft + (tick - this.view.start) / this.perPx;
  }

  tickAt(x: number): number {
    const l = this.layout!;
    return this.view.start + (x - l.plotLeft) * this.perPx;
  }

  hit(x: number, y: number): Hit {
    const l = this.layout;
    if (!l) return { area: "none" };
    if (l.laneHeight > 0 && y >= l.laneTop && y < l.laneTop + l.laneHeight) {
      const lane = Math.floor((y - l.laneTop - 2) / LANE_ROW);
      const note = this.notes.find((n) => n.lane === lane && x >= n.left - 3 && x <= n.right + 3) ?? null;
      return { area: "lane", note };
    }
    const row = l.rows.findIndex((r) => y >= r.top && y < r.top + r.height);
    if (row >= 0) return { area: "row", row };
    if (this.config.axis && y >= l.axisTop - 2) return { area: "axis" };
    return { area: "none" };
  }

  /** The window drawn. Setting it draws there at once, and ends an ease. */
  get view(): { start: number; end: number } {
    return this.#view;
  }

  set view(v: { start: number; end: number }) {
    this.#view = v;
    this.#ease = null;
  }

  /** Where the view is going: an ease's end, or the window drawn. The rows read for it. */
  get target(): { start: number; end: number } {
    return this.#ease?.to ?? this.#view;
  }

  /** Whether the window drawn is easing to the target. */
  get easing(): boolean {
    return this.#ease !== null;
  }

  /** Eases the window drawn from where it is to `to` over `ms`, quick at first, frame by frame. */
  easeTo(to: { start: number; end: number }, ms: number): void {
    this.#ease = { from: { ...this.#view }, to, t0: performance.now(), ms };
    this.invalidate();
  }

  /** The window drawn now: the ease's, or the window drawn where none is under way. An ease that
   * has run its time ends; one under way asks for the next frame. */
  #eased(): void {
    const e = this.#ease;
    if (!e) return;
    const k = Math.min(1, (performance.now() - e.t0) / e.ms);
    if (k >= 1) {
      this.#view = e.to;
      this.#ease = null;
      return;
    }
    const f = 1 - (1 - k) ** 3;
    this.#view = { start: e.from.start + (e.to.start - e.from.start) * f, end: e.from.end + (e.to.end - e.from.end) * f };
    this.invalidate();
  }

  // Frames -------------------------------------------------------------------------------------

  invalidate(): void {
    if (this.#frame) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      this.drawNow();
    });
  }

  /** Whether the view is where it is going, and every row has the data it wants there. */
  get settled(): boolean {
    if (this.#ease) return false;
    const per = this.#perColumn();
    return this.tracks.every((t) => t.settled(this.view.start, this.view.end, per));
  }

  /** Resolves after the first frame drawn with every row's data. */
  whenSettled(): Promise<void> {
    return new Promise((resolve) => {
      this.#settledWaiters.push(resolve);
      this.invalidate();
    });
  }

  #perColumn(): number {
    const l = this.layout;
    if (!l) return 1;
    return (this.view.end - this.view.start) / Math.max(1, Math.round((l.plotRight - l.plotLeft) * this.#dpr));
  }

  drawNow(): void {
    const l = this.layout;
    const p = this.palette;
    if (!l || !p || !this.info) return;
    this.#eased();
    const ctx = this.#ctx;
    const dpr = this.#dpr;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.textBaseline = "alphabetic";

    const ncols = Math.max(1, Math.round((l.plotRight - l.plotLeft) * dpr));
    const grid: Grid = { x0: this.view.start, per: (this.view.end - this.view.start) / ncols, count: ncols };
    // Each row reads for where the view is going, so an ease's step asks for its data at once and
    // draws what it holds until that lands.
    const to = this.target;
    for (const t of this.tracks) t.need(to.start, to.end, (to.end - to.start) / ncols);

    const revealX = this.reveal ?? Infinity;
    const lane = this.laneNotes();
    const noteIns: NoteIn[] = lane.map((a) => ({ id: a.id, start: a.start, end: a.end, label: a.label }));
    const noteFont = this.#font(this.config.labelPx);
    this.notes = this.config.lanes > 0 && l.laneHeight > 0
      ? layoutNotes(noteIns, (t) => this.xAt(t), l.plotLeft, l.plotRight, this.config.lanes, (s) => this.measure(s, noteFont))
      : [];

    // The row under the pointer.
    if (this.hoverRow !== null && l.rows[this.hoverRow]) {
      const r = l.rows[this.hoverRow];
      ctx.fillStyle = p.wash;
      ctx.fillRect(0, r.top, l.width, r.height);
    }
    // Where every note's bounds are drawn, as in the inset, the note under the pointer washes its
    // span across the rows.
    const lit = this.config.noteRules && this.hoverNote !== null ? lane.find((a) => a.id === this.hoverNote) : undefined;
    if (lit && lit.end !== null && l.rows.length > 0) {
      const x0 = Math.max(l.plotLeft, this.xAt(lit.start));
      const x1 = Math.min(l.plotRight, this.xAt(lit.end));
      const last = l.rows[l.rows.length - 1];
      ctx.fillStyle = p.wash;
      if (x1 > x0) ctx.fillRect(x0, l.rows[0].top, x1 - x0, last.top + last.height - l.rows[0].top);
    }

    this.#drawRules(lane, revealX);

    // Rows, each group's under its head.
    this.#drawHeads();
    for (const r of l.rows) this.#drawRow(r, grid, revealX);

    this.#drawPulseMark();
    this.#drawLane(revealX);
    if (this.config.axis) this.#drawAxis();
    this.#drawMarks();

    this.onDraw?.();
    this.#watchStall();
    if (this.#settledWaiters.length > 0 && this.settled) {
      const waiters = this.#settledWaiters;
      this.#settledWaiters = [];
      for (const w of waiters) w();
    }
  }

  #watchStall(): void {
    clearTimeout(this.#stallTimer);
    const since = Math.min(...this.tracks.map((t) => t.waitingSince ?? Infinity));
    if (since === Infinity) {
      if (this.#stalled) {
        this.#stalled = false;
        this.onStall?.(null);
      }
      return;
    }
    const due = since + STALL_MS - performance.now();
    if (due > 0) this.#stallTimer = setTimeout(() => this.#watchStall(), due);
    else if (!this.#stalled) {
      this.#stalled = true;
      this.onStall?.(STALL_MS);
    }
  }

  // Rows ---------------------------------------------------------------------------------------

  /** Each group's name in its band, over a rule that runs across the plot under it. The name may
   * run past the names column: nothing is drawn beside it. */
  #drawHeads(): void {
    const l = this.layout!;
    const p = this.palette!;
    const ctx = this.#ctx;
    const font = this.#font(this.config.labelPx, 600);
    ctx.font = font;
    ctx.textAlign = "left";
    for (const h of l.heads) {
      const base = h.top + h.height - 7;
      ctx.fillStyle = p.ink;
      ctx.fillText(h.group, 6 + this.grip, base, l.plotRight - 6 - this.grip);
      const end = 6 + this.grip + this.measure(h.group, font) + 8;
      if (end >= l.plotRight) continue;
      ctx.strokeStyle = p.hair;
      ctx.lineWidth = 1;
      ctx.beginPath();
      const y = Math.round(base - 4) + 0.5;
      ctx.moveTo(Math.max(end, l.plotLeft), y);
      ctx.lineTo(l.plotRight, y);
      ctx.stroke();
    }
  }

  #drawRow(r: Row, grid: Grid, revealX: number): void {
    const l = this.layout!;
    const p = this.palette!;
    const ctx = this.#ctx;
    const dpr = this.#dpr;
    const c = r.track.channel;

    // The name, and an analog row's scale.
    ctx.fillStyle = this.hoverRow !== null && l.rows[this.hoverRow] === r ? p.ink : p.ink2;
    ctx.font = this.#font(this.config.labelPx, 500);
    ctx.textAlign = "left";
    ctx.fillText(c.name, 6 + this.grip, r.top + r.height / 2 + 4, l.plotLeft - 10 - this.grip);
    if (r.range && c.kind === "analog") {
      // Each label sits on its rule, the top one under it and the bottom one over it.
      const m = r.range.marks;
      const top = this.valueY(r, m.top);
      const bottom = this.valueY(r, m.bottom);
      ctx.font = this.#font(AXIS_PX);
      ctx.textAlign = "right";
      ctx.fillStyle = p.ink2;
      const [topLabel, bottomLabel] = formatMarks(m.top, m.bottom, m.place, c.unit);
      ctx.fillText(topLabel, l.plotLeft - 6, top + 7);
      ctx.fillText(bottomLabel, l.plotLeft - 6, bottom + 2);
      ctx.strokeStyle = p.hair;
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (const y of [Math.round(top + 0.5) + 0.5, Math.round(bottom - 0.5) + 0.5]) {
        ctx.moveTo(l.plotLeft, y);
        ctx.lineTo(l.plotRight, y);
      }
      ctx.stroke();
      this.#shown.delete(r.track.index);
    }
    // A channel this version can't read holds its place, and says so (FORMAT.md, "Unavailable
    // channels").
    if (c.kind === "unavailable") {
      ctx.fillStyle = p.ink2;
      ctx.fillText(UNAVAILABLE, l.plotLeft + 6, r.top + r.height / 2 + 4, l.plotRight - l.plotLeft - 12);
      return;
    }

    // Nothing before the channel's data, at or past its end, or right of the sweep; the samples an
    // analog channel lost before its first and after its last are pale out to the extent.
    const x0 = Math.max(l.plotLeft, this.xAt(channelStart(c)));
    const x1 = Math.min(l.plotRight, this.xAt(c.end), revealX);
    const extent = channelExtent(c);
    const lx0 = Math.max(l.plotLeft, this.xAt(extent.start));
    const lx1 = Math.min(l.plotRight, this.xAt(extent.end), revealX);
    if (lx1 <= lx0) return;
    const width = (weight: number) => Math.max(1, Math.round(weight * dpr));
    // Each layer in device pixels, at each line width a stretch draws at (the shapes align to the
    // width). Neighbours share a rounded edge, so no seam shows between them. A layer with buckets
    // wider than a column is coarse: its analog bands may claim values the view does not hold, so
    // they draw faded. A digital row's shades come from its model alone (shade.ts).
    type Piece = { px0: number; px1: number; shapes: Shapes; coarse: boolean };
    const shown = { lo: Infinity, hi: -Infinity };
    // Off the plot, the trace leaves only the losses at either end to draw.
    const traced = x1 > x0;
    const stretches = traced ? this.#stretches(c.name, Math.round(x0 * dpr), Math.round(x1 * dpr)) : [];
    // A digital row in several stretches takes its line in paths of `LINE_COLUMNS`, each stretch
    // stroking those it meets; in one, a path.
    const columns = stretches.length > 1 ? LINE_COLUMNS : Infinity;
    const byWidth = new Map<number, Piece[]>();
    const piecesAt = (lw: number): Piece[] => {
      let pieces = byWidth.get(lw);
      if (pieces) return pieces;
      // One piece a span of the channel's data, so nothing joins across a gap or a break. A channel
      // whose list is in pages takes each tile's.
      pieces = r.track.layers().flatMap(({ tile, start, end }) => {
        const cc = spanned(c, tile.data.listed);
        return knownSpans(cc, start, end).flatMap(([from, to]) => {
          const px0 = Math.round(Math.max(x0, this.xAt(from)) * dpr);
          const px1 = Math.round(Math.min(x1, this.xAt(to)) * dpr);
          if (px1 <= px0) return [];
          let shapes;
          if (cc.kind === "analog") shapes = this.#analogShapes(r, cc, grid, tile, from, to, lw, shown);
          else if (tile.kind === "edges") {
            // Past a gap in the tile the level is the gap's, not the one its edges carry to there.
            const at = rebased(cc, tile.start, tile.data, from);
            shapes = this.#digitalShapes(r, grid, { ...tile, start: at.start, data: at.edges }, from, to, lw, columns);
          } else shapes = this.#digitalShapes(r, grid, tile, from, to, lw, columns);
          return [{ px0, px1, shapes, coarse: grain(tile) > grid.per }];
        });
      });
      byWidth.set(lw, pieces);
      return pieces;
    };
    if (traced && piecesAt(width(p.lineWeight)).length === 0) return;
    if (shown.lo <= shown.hi) this.#shown.set(r.track.index, { lo: shown.lo, hi: shown.hi });

    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.lineJoin = "miter";
    // Every pixel is painted once, so a faded fill never lands on another colour's.
    for (const s of stretches) {
      const lw = width(s.weight);
      ctx.lineWidth = lw;
      for (const { px0, px1, shapes, coarse } of piecesAt(lw)) {
        const a = Math.max(px0, s.px0);
        const b = Math.min(px1, s.px1);
        if (b <= a) continue;
        ctx.save();
        ctx.beginPath();
        ctx.rect(a, r.top * dpr, b - a, r.height * dpr);
        ctx.clip();
        ctx.strokeStyle = s.colour;
        for (const line of shapes.lines) if (line.x1 > a && line.x0 < b) ctx.stroke(line.path);
        ctx.fillStyle = s.colour;
        if (shapes.fills) this.#drawFills(shapes.fills, s.colour);
        ctx.globalAlpha = coarse ? UNRESOLVED_ALPHA : 1;
        ctx.fill(shapes.fill);
        ctx.restore();
      }
    }
    ctx.restore();

    // Where the instrument lost data: a digital channel's gaps, an analog channel's lost samples. A
    // pause only breaks the trace.
    const lost = lostIn(c, r.track.layers());
    if (lost.length > 0) {
      ctx.save();
      ctx.fillStyle = p.wash;
      for (const g of lost) {
        const a = Math.max(lx0, this.xAt(g.from));
        const b = Math.min(lx1, this.xAt(g.to));
        if (b > a) ctx.fillRect(a, r.top, Math.max(1, b - a), r.height);
      }
      ctx.restore();
    }

    // What a trim leaves out, faded under the page's colour: still there to trim back to.
    const kept = this.kept.get(r.track.index);
    if (kept) {
      ctx.save();
      ctx.globalAlpha = 0.7;
      ctx.fillStyle = p.page;
      for (const [a, b] of [
        [x0, Math.min(x1, this.xAt(kept.from))],
        [Math.max(x0, this.xAt(kept.to)), x1],
      ]) {
        if (b > a) ctx.fillRect(a, r.top, b - a, r.height);
      }
      ctx.restore();
    }
  }

  /** Channel `index` moved by `by` ticks, as `info` now has it: its row's tiles move with it. */
  moveChannel(info: RecordingInfo, index: number, by: number): void {
    this.info = info;
    for (const t of this.tracks) if (t.index === index) t.moveBy(info.channels[index], by);
    this.invalidate();
  }

  /**
   * The device-pixel stretches of a channel's row between px0 and px1, each in the colour and line
   * weight that draw it: the role of the last annotation in the list that paints the channel there
   * (`--tickwave-<role>`, `HEAVY_ROLES`), else the trace's.
   */
  #stretches(channel: string, px0: number, px1: number): { px0: number; px1: number; colour: string; weight: number }[] {
    const p = this.palette!;
    const dpr = this.#dpr;
    const marks: { at: number; i: number; on: boolean }[] = [];
    const roles: string[] = [];
    for (const a of this.annotations?.list ?? []) {
      if (a.paint === null || a.end === null || !covers(a, channel)) continue;
      const from = Math.max(px0, Math.round(this.xAt(a.start) * dpr));
      const to = Math.min(px1, Math.round(this.xAt(a.end) * dpr));
      if (to <= from) continue;
      marks.push({ at: from, i: roles.length, on: true }, { at: to, i: roles.length, on: false });
      roles.push(a.paint);
    }
    // At one x, a span ends before the next begins.
    marks.sort((m, n) => m.at - n.at || Number(m.on) - Number(n.on));
    const out: { px0: number; px1: number; colour: string; weight: number }[] = [];
    const active = new Set<number>();
    const stretch = (from: number, to: number) => {
      const role = active.size === 0 ? null : roles[Math.max(...active)];
      const heavy = role !== null && HEAVY_ROLES.has(role);
      out.push({ px0: from, px1: to, colour: role === null ? p.trace : p.role(role), weight: p.lineWeight + (heavy ? HEAVIER : 0) });
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

  #digitalShapes(r: Row, grid: Grid, tile: Tile, from: number, to: number, lw: number, columns: number): Shapes {
    const l = this.layout!;
    const dpr = this.#dpr;
    let cols: DigitalColumns;
    if (tile.kind === "edges") cols = digitalFromEdges(grid, tile.data, from, to);
    else if (tile.kind === "digital-overview") cols = digitalFromLevels(grid, tile.data, from, to);
    else return { lines: [], fill: new Path2D() };
    const snap = (y: number) => (lw % 2 === 1 ? Math.floor(y) + 0.5 : Math.round(y));
    const yH = snap((r.top + 6) * dpr);
    const yL = snap((r.top + r.height - 6) * dpr);
    const left = Math.round(l.plotLeft * dpr);
    const line = new Lines(left, columns, lw);
    const look = looks(cols, 0, grid.count, lw, grid.per, this.info!.timebase);
    const fills = digitalTrace(cols, look, grid.count, { left, yH, yL, lw }, line);
    const h = lw / 2;
    const layers = [
      [yH + h, yL - yH - lw],
      [yH - h, lw],
      [yL - h, lw],
    ] as const;
    return { lines: line.lines, fill: new Path2D(), fills: fills ? { fills, left, layers } : undefined };
  }

  /**
   * A row's fills in `colour`: each layer a strip of one-pixel columns at their alphas, drawn
   * stretched to the layer's depth. One image a piece, where a path of a rectangle a column cost a
   * browser's rasteriser by the rectangle; and one a piece and colour, so the stretches that
   * annotations split a row into share it. A piece and colour not on a strip takes the one used
   * least lately.
   */
  #drawFills({ fills, left, layers }: Strips, colour: string): void {
    const ctx = this.#ctx;
    const n = fills.band.length;
    const held = this.#strips.findIndex((s) => s.holds === fills && s.colour === colour);
    let strip = held >= 0 ? this.#strips.splice(held, 1)[0] : this.#strips.length < STRIPS ? null : this.#strips.pop()!;
    if (!strip || strip.ctx.canvas.width !== n) {
      const canvas = Object.assign(document.createElement("canvas"), { width: n, height: layers.length });
      strip = { ctx: canvas.getContext("2d")!, holds: null, colour };
    }
    this.#strips.unshift(strip);
    if (strip.holds !== fills || strip.colour !== colour) {
      const [r, g, b, a] = rgba(colour);
      const image = new ImageData(n, layers.length);
      const d = image.data;
      [fills.band, fills.high, fills.low].forEach((alpha, row) => {
        for (let c = 0, o = 4 * row * n; c < n; c++, o += 4) {
          if (alpha[c] === 0) continue;
          d[o] = r;
          d[o + 1] = g;
          d[o + 2] = b;
          d[o + 3] = a * alpha[c];
        }
      });
      strip.ctx.putImageData(image, 0, 0);
      [strip.holds, strip.colour] = [fills, colour];
    }
    ctx.save();
    ctx.imageSmoothingEnabled = false;
    layers.forEach(([y, depth], row) => {
      if (depth > 0) ctx.drawImage(strip.ctx.canvas, 0, row, n, 1, left, y, n, depth);
    });
    ctx.restore();
  }

  #analogShapes(
    r: Row,
    c: AnalogChannel,
    grid: Grid,
    tile: Tile,
    from: number,
    to: number,
    lw: number,
    shown: { lo: number; hi: number },
  ): Shapes {
    const l = this.layout!;
    const dpr = this.#dpr;
    const range = r.range!;
    const top = (r.top + 6) * dpr;
    const bottom = (r.top + r.height - 6) * dpr;
    const y = (v: number) => bottom - ((v - range.lo) / (range.hi - range.lo)) * (bottom - top);
    const left = l.plotLeft * dpr;
    const lines: Line[] = [];
    const fill = new Path2D();
    const period = axisPeriod(c);
    if (tile.kind === "samples" && grid.per < period) {
      // Fewer samples than columns: a line through the samples, with a dot on each once they
      // stand apart.
      const s = tile.data;
      let started = false;
      const dots = period / grid.per >= 8 * dpr;
      // A held tile can be far wider than the view. The pad, ten line widths, is twice the reach
      // of a miter join at the default limit, and more than a dot's radius.
      const [k0, k1] = lineSamples(grid, s, c, from, to, 10 * lw * grid.per);
      const tickOf = sampleTicks(c);
      // A path ends halfway along a segment and the next starts there, so each join is in one path.
      // The pixels the two butt ends share come out a shade lighter. A miter join at the default
      // limit reaches five line widths from its point.
      const reach = 5 * lw;
      const path = (): Line => ({ path: new Path2D(), x0: Infinity, x1: -Infinity });
      const point = (l: Line, x: number) => {
        l.x0 = Math.min(l.x0, x - reach);
        l.x1 = Math.max(l.x1, x + reach);
      };
      let line = path();
      let segments = 0;
      let lx = 0;
      let ly = 0;
      for (let k = k0; k < k1; k++) {
        if (Number.isNaN(s.values[k])) continue;
        const t = tickOf(s.first + k);
        const x = left + (t - grid.x0) / grid.per;
        if (t >= this.view.start && t <= this.view.end) {
          shown.lo = Math.min(shown.lo, s.values[k]);
          shown.hi = Math.max(shown.hi, s.values[k]);
        }
        const yy = y(s.values[k]);
        if (!started) {
          lines.push(line);
          line.path.moveTo(x, yy);
        } else if (segments === LINE_SEGMENTS) {
          const [mx, my] = [(lx + x) / 2, (ly + yy) / 2];
          line.path.lineTo(mx, my);
          point(line, mx);
          line = path();
          lines.push(line);
          line.path.moveTo(mx, my);
          point(line, mx);
          line.path.lineTo(x, yy);
          segments = 1;
        } else {
          line.path.lineTo(x, yy);
          segments++;
        }
        point(line, x);
        started = true;
        lx = x;
        ly = yy;
        if (dots) {
          fill.moveTo(x + lw, yy);
          fill.arc(x, yy, lw, 0, 2 * Math.PI);
        }
      }
      return { lines, fill };
    }
    let cols: AnalogColumns;
    if (tile.kind === "samples") cols = analogFromSamples(grid, tile.data, c, from, to);
    else if (tile.kind === "analog-overview") cols = analogFromOverview(grid, tile.data, from, to);
    else return { lines, fill };
    // Each run of columns with a band is one outline, along the bands' tops and back along their
    // bottoms, with a corner only where a band's end moves: the pixels of a rectangle a column,
    // which Firefox fills far slower.
    const x0 = Math.round(left);
    for (let k = 0; k < grid.count; ) {
      if (cols.min[k] > cols.max[k]) {
        k++;
        continue;
      }
      let top = y(cols.max[k]) - lw / 2;
      fill.moveTo(x0 + k, top);
      let j = k;
      for (; j < grid.count && cols.min[j] <= cols.max[j]; j++) {
        shown.lo = Math.min(shown.lo, cols.min[j]);
        shown.hi = Math.max(shown.hi, cols.max[j]);
        const t = y(cols.max[j]) - lw / 2;
        if (t !== top) {
          fill.lineTo(x0 + j, top);
          fill.lineTo(x0 + j, t);
          top = t;
        }
      }
      let bottom = y(cols.min[j - 1]) + lw / 2;
      fill.lineTo(x0 + j, top);
      fill.lineTo(x0 + j, bottom);
      for (let m = j - 2; m >= k; m--) {
        const b = y(cols.min[m]) + lw / 2;
        if (b !== bottom) {
          fill.lineTo(x0 + m + 1, bottom);
          fill.lineTo(x0 + m + 1, b);
          bottom = b;
        }
      }
      fill.lineTo(x0 + k, bottom);
      fill.closePath();
      k = j;
    }
    return { lines, fill };
  }

  // The pulse under the pointer ----------------------------------------------------------------

  /**
   * Where a mark's spans and figures go. The width's span runs between the pulse's edges just
   * inside its level; the period's runs from its start to the closing edge just inside the other
   * level. A figure sits on its span, over the stretch where the trace leaves room (the pulse for
   * the width, the rest of the period for the period), when the row and that stretch are big enough.
   */
  #markGeometry(m: PulseMark) {
    const l = this.layout;
    const r = l?.rows[m.row];
    if (!l || !r) return null;
    const yH = r.top + 6;
    const yL = r.top + r.height - 6;
    const roomy = yL - yH >= FIGURES_ROOM;
    const d = roomy ? FIGURE_BOX / 2 + 2 : 3;
    const font = this.#font(this.config.labelPx);
    const x = (t: number) => this.xAt(t);
    const visible = (a: number, b: number) => ({ a: Math.max(l.plotLeft, a), b: Math.min(l.plotRight, b) });
    const figure = (text: string | null, over: { a: number; b: number }, y: number) => {
      if (!text || !roomy) return null;
      const w = this.measure(text, font) + 2 * FIGURE_PAD;
      return over.b - over.a >= w + 2 * ARROW + 4 ? { text, x: (over.a + over.b) / 2, y, w } : null;
    };
    const near = m.level ? yH + d : yL - d;
    const far = m.level ? yL - d : yH + d;
    const width = { x0: x(m.start), x1: x(m.end), y: near, figure: figure(m.width, visible(x(m.start), x(m.end)), near) };
    const period =
      m.next === null
        ? null
        : { x0: x(m.start), x1: x(m.next), y: far, figure: figure(m.period, visible(x(m.end), x(m.next)), far) };
    return { width, period, font };
  }

  /** Which of a mark's figures sit on their spans: the others are the caller's to show. */
  markFits(m: PulseMark): { width: boolean; period: boolean } {
    const g = this.#markGeometry(m);
    return { width: !!g?.width.figure, period: !!g?.period?.figure };
  }

  #drawPulseMark(): void {
    const m = this.pulseMark;
    const g = m && this.#markGeometry(m);
    if (!g) return;
    const l = this.layout!;
    const p = this.palette!;
    const ctx = this.#ctx;
    ctx.save();
    ctx.beginPath();
    ctx.rect(l.plotLeft, 0, l.plotRight - l.plotLeft, l.height);
    ctx.clip();
    ctx.strokeStyle = p.ink;
    ctx.fillStyle = p.ink;
    ctx.lineWidth = 1;
    for (const s of [g.width, g.period]) {
      if (!s) continue;
      const y = Math.round(s.y) + 0.5;
      // An end far off the plot is drawn just past its side, where the clip hides it.
      const off = ARROW + 2;
      const a = Math.max(l.plotLeft - off, Math.round(s.x0) + 1);
      const b = Math.min(l.plotRight + off, Math.round(s.x1) - 1);
      if (b <= a) continue;
      ctx.beginPath();
      ctx.moveTo(a, y);
      ctx.lineTo(b, y);
      ctx.stroke();
      // Heads that point at the edges, where the span is wide enough to show them.
      if (s.x1 - s.x0 >= 2 * ARROW + 6) {
        ctx.beginPath();
        ctx.moveTo(a, y);
        ctx.lineTo(a + ARROW, y - 3);
        ctx.lineTo(a + ARROW, y + 3);
        ctx.closePath();
        ctx.moveTo(b, y);
        ctx.lineTo(b - ARROW, y - 3);
        ctx.lineTo(b - ARROW, y + 3);
        ctx.closePath();
        ctx.fill();
      }
    }
    ctx.font = g.font;
    ctx.textAlign = "center";
    for (const f of [g.width.figure, g.period?.figure]) {
      if (!f) continue;
      ctx.fillStyle = p.page;
      ctx.globalAlpha = FIGURE_ALPHA;
      ctx.fillRect(Math.round(f.x - f.w / 2), Math.round(f.y - FIGURE_BOX / 2), Math.round(f.w), FIGURE_BOX);
      ctx.globalAlpha = 1;
      ctx.fillStyle = p.ink;
      ctx.fillText(f.text, f.x, f.y + 4);
    }
    ctx.restore();
  }

  // Annotations, axis, marks -------------------------------------------------------------------

  /** Hair rules at note boundaries: every note in the inset, the emphasised one in a view. */
  #drawRules(lane: Annotation[], revealX: number): void {
    const l = this.layout!;
    const ctx = this.#ctx;
    const shown = this.config.noteRules ? lane : lane.filter((a) => a.id === this.focus || a.id === this.hoverNote);
    if (shown.length === 0) return;
    const bottom = l.rows.length > 0 ? l.rows[l.rows.length - 1].top + l.rows[l.rows.length - 1].height : l.laneTop + l.laneHeight;
    const xs = new Set<number>();
    for (const a of shown) {
      xs.add(Math.round(this.xAt(a.start)));
      if (a.end !== null) xs.add(Math.round(this.xAt(a.end)));
    }
    ctx.strokeStyle = this.palette!.hair;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (const x of xs) {
      if (x < l.plotLeft || x > l.plotRight || x > revealX) continue;
      ctx.moveTo(x + 0.5, l.laneTop + l.laneHeight);
      ctx.lineTo(x + 0.5, bottom);
    }
    ctx.stroke();
  }

  #drawLane(revealX: number): void {
    const l = this.layout!;
    const p = this.palette!;
    const ctx = this.#ctx;
    // While a sweep plays, the notes draw on with the traces, each as far as the sweep has reached.
    ctx.save();
    if (revealX < Infinity) {
      ctx.beginPath();
      ctx.rect(0, l.laneTop, revealX, l.laneHeight);
      ctx.clip();
    }
    for (const n of this.notes) {
      if (n.left > revealX) continue;
      const y = l.laneTop + 2 + n.lane * LANE_ROW;
      const emphasised = n.id === this.focus;
      if (n.id === this.hoverNote) {
        ctx.fillStyle = p.wash;
        ctx.fillRect(n.left - 3, y, n.right - n.left + 6, LANE_ROW);
      }
      ctx.strokeStyle = emphasised ? p.ink : p.ink2;
      ctx.lineWidth = 1;
      ctx.beginPath();
      const by = Math.round(y + LANE_ROW - 4) + 0.5;
      if (n.point) {
        ctx.moveTo(Math.round(n.x0) + 0.5, y + 3);
        ctx.lineTo(Math.round(n.x0) + 0.5, y + LANE_ROW);
      } else {
        const a = Math.round(n.x0) + 0.5;
        const b = Math.max(a, Math.round(n.x1) - 0.5);
        ctx.moveTo(a, by + 3);
        ctx.lineTo(a, by);
        ctx.lineTo(b, by);
        ctx.lineTo(b, by + 3);
      }
      ctx.stroke();
      if (n.label) {
        ctx.font = this.#font(this.config.labelPx, emphasised ? 600 : 400);
        ctx.fillStyle = emphasised ? p.ink : p.ink2;
        ctx.textAlign = "left";
        ctx.fillText(n.label, n.labelX, y + LANE_ROW - 7);
      }
    }
    ctx.restore();
    // In the first sub-lane, where a label sits after the start of its note.
    const say = this.laneSays();
    if (say !== null) {
      ctx.font = this.#font(this.config.labelPx, 400);
      ctx.fillStyle = p.ink2;
      ctx.textAlign = "left";
      ctx.fillText(say, l.plotLeft + 4, l.laneTop + 2 + LANE_ROW - 7);
    }
  }

  #drawAxis(): void {
    const l = this.layout!;
    const p = this.palette!;
    const ctx = this.#ctx;
    const font = this.#font(AXIS_PX);
    const width = l.plotRight - l.plotLeft;
    const axis = timeAxis(this.view.start, this.view.end, width, this.info!.timebase, (s) => this.measure(s, font));
    const y = l.axisTop + 0.5;
    ctx.strokeStyle = p.hair;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(l.plotLeft, y);
    ctx.lineTo(l.plotRight, y);
    ctx.stroke();
    const sel = this.selection;
    const sx0 = sel ? Math.max(l.plotLeft, this.xAt(Math.min(sel.start, sel.end))) : 0;
    const sx1 = sel ? Math.min(l.plotRight, this.xAt(Math.max(sel.start, sel.end))) : 0;
    if (sel && sx1 > sx0) {
      // The selection: an ink bar along the axis, above the labels, which stay as they are.
      ctx.fillStyle = p.ink;
      ctx.fillRect(sx0, l.axisTop, Math.max(1, sx1 - sx0), SELECTION_BAR);
    }
    // Cursor flags sit in the axis band; a label they would cover is left out.
    const flags = this.#flags();
    const ticks = axis.ticks.filter((t) => {
      const x = l.plotLeft + t.x;
      return !flags.some((f) => x + t.width / 2 + 4 > f.x && x - t.width / 2 - 4 < f.x + f.w);
    });
    ctx.font = font;
    ctx.textAlign = "left";
    ctx.beginPath();
    for (const t of ticks) {
      const x = Math.round(l.plotLeft + t.x) + 0.5;
      ctx.moveTo(x, l.axisTop);
      ctx.lineTo(x, l.axisTop + 4);
    }
    ctx.strokeStyle = p.ink2;
    ctx.stroke();
    ctx.fillStyle = p.ink2;
    for (const t of ticks) ctx.fillText(t.label, l.plotLeft + t.x - t.width / 2, l.axisTop + 16);
  }

  /** Where the cursor flags go: an ink fill with page text, in the axis band. */
  #flags(): { name: string; x: number; w: number }[] {
    const l = this.layout!;
    const font = this.#font(AXIS_PX, 600);
    const out: { name: string; x: number; w: number }[] = [];
    for (const [name, tick] of [["A", this.cursors.a], ["B", this.cursors.b]] as const) {
      if (tick === null) continue;
      const x = Math.round(this.xAt(tick)) + 0.5;
      if (x < l.plotLeft || x > l.plotRight) continue;
      const w = this.measure(name, font) + 8;
      out.push({ name, x: Math.min(x - 0.5, l.plotRight - w), w });
    }
    return out;
  }

  #drawMarks(): void {
    const l = this.layout!;
    const p = this.palette!;
    const ctx = this.#ctx;
    const top = l.laneTop + l.laneHeight;
    const bottom = this.config.axis ? l.axisTop : l.height;
    const vline = (tick: number) => {
      const x = Math.round(this.xAt(tick)) + 0.5;
      if (x < l.plotLeft || x > l.plotRight) return;
      ctx.beginPath();
      ctx.moveTo(x, top);
      ctx.lineTo(x, bottom);
      ctx.stroke();
    };
    ctx.strokeStyle = p.ink;
    ctx.lineWidth = 1;
    const sel = this.selection;
    if (sel) {
      vline(sel.start);
      vline(sel.end);
    }
    for (const tick of [this.cursors.a, this.cursors.b]) if (tick !== null) vline(tick);
    if (!this.config.axis) return;
    ctx.font = this.#font(AXIS_PX, 600);
    ctx.textAlign = "left";
    for (const f of this.#flags()) {
      ctx.fillStyle = p.ink;
      ctx.fillRect(f.x, l.axisTop + 2, f.w, 16);
      ctx.fillStyle = p.page;
      ctx.fillText(f.name, f.x + 4, l.axisTop + 14);
    }
  }
}

export function channelIndices(info: RecordingInfo, names: readonly string[] | null): { indices: number[]; missing: string[] } {
  if (names === null) return { indices: info.channels.map((_, i) => i), missing: [] };
  const indices: number[] = [];
  const missing: string[] = [];
  for (const n of names) {
    const i = info.channels.findIndex((c: Channel) => c.name === n);
    if (i < 0) missing.push(n);
    else indices.push(i);
  }
  return { indices, missing };
}
