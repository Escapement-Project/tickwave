// The canvas renderer: an annotation lane, one row a channel, and a time axis, drawn at the device
// pixel ratio. It draws whatever data has arrived and asks for more; a fetch that completes
// schedules another frame.

import { covers, laneAnnotations, type Annotation, type Resolved } from "../annotations.ts";
import type { Palette } from "../palette.ts";
import { formatAt, niceStep } from "../readouts.ts";
import { named, sampleTick, type AnalogChannel, type Channel, type RecordingInfo, type Source } from "../source.ts";
import { timeAxis } from "./axis.ts";
import {
  analogFromOverview,
  analogFromSamples,
  BUSY,
  digitalFromEdges,
  digitalFromOverview,
  FALL,
  HIGH,
  LOW,
  NONE,
  RISE,
  type AnalogColumns,
  type DigitalColumns,
  type Grid,
} from "./columns.ts";
import { layoutNotes, type NoteIn, type NoteOut } from "./notes.ts";
import { grain, Track, type Tile } from "./track.ts";

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
  /** For an analog row, the values at its bottom and top, and the place they are labelled at. */
  readonly range?: { readonly lo: number; readonly hi: number; readonly place: number };
}

export interface Layout {
  readonly width: number;
  readonly height: number;
  readonly plotLeft: number;
  readonly plotRight: number;
  readonly laneTop: number;
  readonly laneHeight: number;
  readonly rows: readonly Row[];
  readonly axisTop: number;
}

const AXIS_PX = 11;
const LANE_ROW = 18;
const AXIS_HEIGHT = 24;
/** The selection's bar along the top of the axis band, clear of the labels below it. */
const SELECTION_BAR = 4;
const RIGHT_PAD = 10;
/** The opacity of what a coarse layer cannot place: the trace's colour, faded. */
const UNRESOLVED_ALPHA = 0.35;
/** A trace's line, in CSS pixels. */
const TRACE_WEIGHT = 1.5;
/** A paint role that draws heavier than a trace, so its lines differ from the others' by more than
 * colour: the target's drive. */
export const ROLE_WEIGHT: Readonly<Record<string, number>> = { target: 2.5 };
/** How long a read may run before the element says it is still loading. */
const STALL_MS = 5000;

/** A failed read as the reader meets it: data that did not load. The name stays, for a page to tell the cause. */
function notLoaded(e: unknown): Error {
  const out = named(e instanceof Error ? e.name : "Error", `Data did not load: ${e instanceof Error ? e.message : String(e)}`);
  out.cause = e;
  return out;
}

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
  #palette: Palette | null = null;
  layout: Layout | null = null;
  view = { start: 0, end: 1 };
  cursors: { a: number | null; b: number | null } = { a: null, b: null };
  selection: { start: number; end: number } | null = null;
  hoverRow: number | null = null;
  hoverNote: string | null = null;
  /** Row heights the reader set, by channel index; the others take the config's. */
  heights = new Map<number, number>();
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
    this.tracks = channels.map(
      (i) =>
        new Track(
          source,
          i,
          info.channels[i],
          () => {
            this.#recovered();
            this.invalidate();
          },
          (e) => {
            this.#watchStall();
            const error = notLoaded(e);
            this.#notLoaded.add(error.message);
            this.onError?.(error);
          },
        ),
    );
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

  #analogRange(c: AnalogChannel): { lo: number; hi: number; place: number } {
    const lo0 = Number.isFinite(c.min) ? c.min : 0;
    const hi0 = Number.isFinite(c.max) ? Math.max(c.max, lo0) : lo0;
    const spread = hi0 > lo0 ? hi0 - lo0 : Math.max(c.resolution ?? 0, Math.abs(hi0) * 1e-3, 1e-12);
    const { step, place } = niceStep(spread / 4);
    return { lo: Math.floor(lo0 / step) * step, hi: Math.max(Math.ceil(hi0 / step) * step, Math.floor(lo0 / step) * step + step), place };
  }

  defaultHeight(t: Track): number {
    return t.channel.kind === "analog" ? this.config.analogHeight : this.config.rowHeight;
  }

  /** The lane, the rows at their own heights and the axis: what the plot stacks, with no height given. */
  #stack(): { laneHeight: number; top: number; natural: number[]; fixed: number } {
    const laneHeight = this.config.lanes > 0 && this.laneNotes().length > 0 ? this.config.lanes * LANE_ROW + 4 : 0;
    const top = 2 + laneHeight + (laneHeight > 0 ? 2 : 4);
    const natural = this.tracks.map((t) => this.heights.get(t.index) ?? this.defaultHeight(t));
    const gaps = this.tracks.filter((t, i) => i > 0 && t.channel.kind === "analog").length * 8;
    const fixed = top + gaps + natural.reduce((a, b) => a + b, 0) + (this.config.axis ? AXIS_HEIGHT + 2 : 4);
    return { laneHeight, top, natural, fixed };
  }

  /** Lays the plot out at `width` CSS pixels; `height` above the natural one goes to the rows. */
  resize(width: number, height: number | null): Layout {
    const font = this.#font(this.config.labelPx, 500);
    let gutter = 0;
    const ranges = this.tracks.map((t) => (t.channel.kind === "analog" ? this.#analogRange(t.channel) : undefined));
    this.tracks.forEach((t, i) => {
      gutter = Math.max(gutter, this.measure(t.channel.name, font));
      const r = ranges[i];
      const c = t.channel;
      if (r && c.kind === "analog") {
        const axisFont = this.#font(AXIS_PX);
        gutter = Math.max(gutter, this.measure(formatAt(r.hi, r.place, c.unit), axisFont), this.measure(formatAt(r.lo, r.place, c.unit), axisFont));
      }
    });
    const plotLeft = Math.ceil(Math.min(Math.max(gutter + 14, 40), Math.max(40, width * 0.3)));
    const laneTop = 2;
    const { laneHeight, top: rowsTop, natural, fixed } = this.#stack();
    let top = rowsTop;
    const extra = height !== null ? Math.max(0, height - fixed) : 0;
    const analogs = this.tracks.filter((t) => t.channel.kind === "analog").length;
    const rows: Row[] = this.tracks.map((track, i) => {
      // An analog row's scale labels sit at its top and bottom edges; keep them off the row above.
      if (track.channel.kind === "analog" && i > 0) top += 8;
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
      axisTop,
    };
    this.#dpr = Math.max(1, globalThis.devicePixelRatio || 1);
    this.canvas.width = Math.round(width * this.#dpr);
    this.canvas.height = Math.round(this.layout.height * this.#dpr);
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${this.layout.height}px`;
    this.invalidate();
    return this.layout;
  }

  // Coordinates --------------------------------------------------------------------------------

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

  // Frames -------------------------------------------------------------------------------------

  invalidate(): void {
    if (this.#frame) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      this.drawNow();
    });
  }

  /** Whether every row has the data its current view wants. */
  get settled(): boolean {
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
    const ctx = this.#ctx;
    const dpr = this.#dpr;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.textBaseline = "alphabetic";

    const ncols = Math.max(1, Math.round((l.plotRight - l.plotLeft) * dpr));
    const grid: Grid = { x0: this.view.start, per: (this.view.end - this.view.start) / ncols, count: ncols };
    for (const t of this.tracks) t.need(this.view.start, this.view.end, grid.per);

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

    this.#drawRules(lane, revealX);

    // Rows.
    for (const r of l.rows) this.#drawRow(r, grid, revealX);

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
    ctx.fillText(c.name, 6, r.top + r.height / 2 + 4, l.plotLeft - 10);
    if (r.range && c.kind === "analog") {
      ctx.font = this.#font(AXIS_PX);
      ctx.textAlign = "right";
      ctx.fillStyle = p.ink2;
      ctx.fillText(formatAt(r.range.hi, r.range.place, c.unit), l.plotLeft - 6, r.top + 13);
      ctx.fillText(formatAt(r.range.lo, r.range.place, c.unit), l.plotLeft - 6, r.top + r.height - 4);
      ctx.strokeStyle = p.hair;
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (const y of [r.top + 6.5, r.top + r.height - 6.5]) {
        ctx.moveTo(l.plotLeft, Math.round(y) + 0.5);
        ctx.lineTo(l.plotRight, Math.round(y) + 0.5);
      }
      ctx.stroke();
    }

    // Nothing before tick 0, at or past the channel's end, or right of the sweep.
    const x0 = Math.max(l.plotLeft, this.xAt(0));
    const x1 = Math.min(l.plotRight, this.xAt(c.end), revealX);
    if (x1 <= x0) return;
    const width = (weight: number) => Math.max(1, Math.round(weight * dpr));
    // Each layer in device pixels, at each line width a stretch draws at (the shapes align to the
    // width). Neighbours share a rounded edge, so no seam shows between them. A layer with buckets
    // wider than a column is coarse: its busy runs and analog bands may claim activity the view
    // does not hold, so they draw faded. Its flat buckets are exact at any scale and draw as they are.
    type Piece = { px0: number; px1: number; shapes: { line: Path2D; fill: Path2D }; coarse: boolean };
    const byWidth = new Map<number, Piece[]>();
    const piecesAt = (lw: number): Piece[] => {
      let pieces = byWidth.get(lw);
      if (pieces) return pieces;
      pieces = r.track.layers().flatMap(({ tile, start, end }) => {
        const px0 = Math.round(Math.max(x0, this.xAt(start)) * dpr);
        const px1 = Math.round(Math.min(x1, this.xAt(end)) * dpr);
        if (px1 <= px0) return [];
        const from = Math.max(start, 0);
        const to = Math.min(end, c.end);
        const shapes = c.kind === "digital" ? this.#digitalShapes(r, grid, tile, from, to, lw) : this.#analogShapes(r, c, grid, tile, from, to, lw);
        return [{ px0, px1, shapes, coarse: grain(tile) > grid.per }];
      });
      byWidth.set(lw, pieces);
      return pieces;
    };
    if (piecesAt(width(TRACE_WEIGHT)).length === 0) return;

    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.lineJoin = "miter";
    // Every pixel is painted once, so a faded fill never lands on another colour's.
    for (const s of this.#stretches(c.name, Math.round(x0 * dpr), Math.round(x1 * dpr))) {
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
        ctx.stroke(shapes.line);
        ctx.fillStyle = s.colour;
        if (coarse) ctx.globalAlpha = UNRESOLVED_ALPHA;
        ctx.fill(shapes.fill);
        ctx.restore();
      }
    }
    ctx.restore();
  }

  /**
   * The device-pixel stretches of a channel's row between px0 and px1, each in the colour and line
   * weight that draw it: the role of the last annotation in the list that paints the channel there
   * (`--tickwave-<role>`, `ROLE_WEIGHT`), else the trace's.
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
      out.push({ px0: from, px1: to, colour: role === null ? p.trace : p.role(role), weight: (role !== null && ROLE_WEIGHT[role]) || TRACE_WEIGHT });
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

  #digitalShapes(r: Row, grid: Grid, tile: Tile, from: number, to: number, lw: number): { line: Path2D; fill: Path2D } {
    const l = this.layout!;
    const dpr = this.#dpr;
    let cols: DigitalColumns;
    if (tile.kind === "edges") cols = digitalFromEdges(grid, tile.data, from, to);
    else if (tile.kind === "digital-overview") cols = digitalFromOverview(grid, tile.data, from, to);
    else return { line: new Path2D(), fill: new Path2D() };
    const snap = (y: number) => (lw % 2 === 1 ? Math.floor(y) + 0.5 : Math.round(y));
    const yH = snap((r.top + 6) * dpr);
    const yL = snap((r.top + r.height - 6) * dpr);
    const left = Math.round(l.plotLeft * dpr);
    const line = new Path2D();
    const fill = new Path2D();
    let pen: number | null = null;
    let busyFrom = -1;
    const pad = (lw - 1) / 2;
    const flushBusy = (c: number) => {
      if (busyFrom < 0) return;
      fill.rect(left + busyFrom - pad, yH - lw / 2, c - busyFrom + 2 * pad, yL - yH + lw);
      busyFrom = -1;
    };
    for (let c = 0; c < grid.count; c++) {
      const st = cols.state[c];
      const x = left + c;
      if (st !== BUSY) flushBusy(c);
      if (st === NONE) {
        pen = null;
      } else if (st === LOW || st === HIGH) {
        const y = st === HIGH ? yH : yL;
        if (pen !== y) line.moveTo(x, y);
        line.lineTo(x + 1, y);
        pen = y;
      } else if (st === RISE || st === FALL) {
        const ya = st === RISE ? yL : yH;
        const yb = st === RISE ? yH : yL;
        const ex = x + cols.at[c];
        if (pen !== ya) line.moveTo(x, ya);
        line.lineTo(ex, ya);
        line.lineTo(ex, yb);
        line.lineTo(x + 1, yb);
        pen = yb;
      } else {
        if (busyFrom < 0) busyFrom = c;
        pen = null;
      }
    }
    flushBusy(grid.count);
    return { line, fill };
  }

  #analogShapes(
    r: Row,
    c: AnalogChannel,
    grid: Grid,
    tile: Tile,
    from: number,
    to: number,
    lw: number,
  ): { line: Path2D; fill: Path2D } {
    const l = this.layout!;
    const dpr = this.#dpr;
    const range = r.range!;
    const top = (r.top + 6) * dpr;
    const bottom = (r.top + r.height - 6) * dpr;
    const y = (v: number) => bottom - ((v - range.lo) / (range.hi - range.lo)) * (bottom - top);
    const left = l.plotLeft * dpr;
    const line = new Path2D();
    const fill = new Path2D();
    const period = c.period.num / c.period.den;
    if (tile.kind === "samples" && grid.per < period) {
      // Fewer samples than columns: a line through the samples, with a dot on each once they
      // stand apart.
      const s = tile.data;
      let started = false;
      const dots = period / grid.per >= 8 * dpr;
      for (let k = 0; k < s.values.length; k++) {
        const t = sampleTick(c.first, c.period, s.first + k);
        if (t < from || t >= to || Number.isNaN(s.values[k])) continue;
        const x = left + (t - grid.x0) / grid.per;
        const yy = y(s.values[k]);
        if (started) line.lineTo(x, yy);
        else line.moveTo(x, yy);
        started = true;
        if (dots) {
          fill.moveTo(x + lw, yy);
          fill.arc(x, yy, lw, 0, 2 * Math.PI);
        }
      }
      return { line, fill };
    }
    let cols: AnalogColumns;
    if (tile.kind === "samples") cols = analogFromSamples(grid, tile.data, c.first, c.period, from, to);
    else if (tile.kind === "analog-overview") cols = analogFromOverview(grid, tile.data, from, to);
    else return { line, fill };
    for (let k = 0; k < grid.count; k++) {
      if (cols.min[k] > cols.max[k]) continue;
      const y1 = y(cols.max[k]);
      const y2 = y(cols.min[k]);
      fill.rect(Math.round(left) + k, y1 - lw / 2, 1, y2 - y1 + lw);
    }
    return { line, fill };
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
    for (const n of this.notes) {
      if (n.right > revealX) continue;
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
