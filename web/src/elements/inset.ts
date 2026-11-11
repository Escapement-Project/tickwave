// <tickwave-inset>: one annotation's span, magnified, with its label, the annotations inside it as
// notes, and an optional `measure` readout. It sweeps in left to right once per appearance. A reader
// can point at a note or a pulse to read it, step through the notes by keys, and open the recording
// at a note: in a view on the page (`for`) or on another page (`href`).

import { covers, type Annotation } from "../annotations.ts";
import { INSET_ATTRIBUTES, parseChannels, parsePad, parseScale, parseSweep } from "../attributes.ts";
import { escapeHtml as esc } from "../escape.ts";
import { reducedMotion } from "../face.ts";
import { Prober, type Probed } from "../prober.ts";
import { formatInterval, measureReading } from "../readouts.ts";
import { channelIndices, Plot, ROLE_WEIGHT } from "../render/plot.ts";
import { isAbort, named, type RecordingInfo } from "../source.ts";
import { secondsValue, toTicks } from "../time.ts";
import { TickwaveElement, loadRecording, type Loaded } from "./base.ts";
import type { TickwaveView } from "./view.ts";

const CSS = `
:host { max-width: 100%; }
figure { margin: 0; }
figcaption { margin: 0 4px 2px; font-size: 12px; font-weight: 600; line-height: 1.35; }
.text { margin: 0 4px 4px; font-size: 12px; line-height: 1.4; color: var(--ink-2, GrayText); }
.text[hidden] { display: none; }
.frame { position: relative; }
canvas { display: block; }
/* Over the plot: what the pointer and the keys reach. A link where the inset opens another page. */
.hit { position: absolute; inset: 0; display: block; border-radius: 4px; -webkit-tap-highlight-color: transparent; }
.hit[hidden] { display: none; }
.hit:focus { outline: none; }
.hit:focus-visible { outline: var(--focus-width, 2px) solid var(--ink, CanvasText); outline-offset: 2px; }
.hit.opens { cursor: pointer; }
.foot { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: baseline; gap: 2px 14px;
  margin: 2px 4px 0; font-size: 12px; color: var(--ink-2, GrayText); }
.foot:empty { display: none; }
/* The first line: the measure, or while the reader points at a part, its reading, whole items only.
   It is held once the annotation asks for a measure or the inset can be pointed at, and the key has
   a line of its own: the inset's height is set when its sidecar arrives, and neither its data nor
   the pointer moves anything. */
.line { flex: 1 1 auto; min-width: 0; overflow: hidden; white-space: nowrap; }
.line::before { content: "\\200b"; }
.line[hidden], .measure[hidden], .key:empty, .line.reads .measure, .line:not(.reads) .reading { display: none; }
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
    hit.addEventListener("pointerdown", (e) => {
      this.#touch = e.pointerType === "touch";
      this.#litAtTouch = this.#lit;
      this.#point(e);
    });
    hit.addEventListener("pointermove", (e) => this.#point(e));
    // A finger's pointer leaves as it lifts: what it lit stays until the next tap.
    hit.addEventListener("pointerleave", (e) => {
      if (e.pointerType !== "touch") this.clear();
    });
    hit.addEventListener("click", (e) => this.#clicked(e));
    hit.addEventListener("keydown", (e) => this.#keyed(e));
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
    if (!this.#visible) {
      this.#visible = true;
      // A tap can close and reopen an inset as the finger lifts; that is not a new appearance.
      this.#sweepWanted = performance.now() - this.#hiddenAt >= REAPPEAR_MS;
      if (this.#sweepWanted) this.#plot.reveal = this.#plot.layout?.plotLeft ?? 0;
      else this.#plot.reveal = null;
    }
    const width = this.#frame.clientWidth;
    if (width > 0 && width !== this.#width) {
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
    if (w) this.#plot.view = w;
    if (this.#sweepWanted && this.#plot.reveal !== null) this.#plot.reveal = this.#plot.layout!.plotLeft;
    this.#plot.invalidate();
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
      const text = measureText(a.measure.what, a.measure.channel, edges.level, edges.ticks, l.info);
      if (text === null) this.fail(new Error(`annotation ${a.id}: cannot measure ${a.measure.what}`));
      else this.#measure.innerHTML = text;
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
   * `href` with the recording, its sidecar and the annotation `id` as its parameters, the others it
   * has kept. A recording on the same site goes in by its path, relative where it lies under that
   * page's folder; another site's whole.
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
    if (this.#plot.laneNotes().length > 0) hit.setAttribute("aria-description", "The left and right arrow keys step through its parts.");
    else hit.removeAttribute("aria-description");
  }

  /** What lies under the pointer: a note lights, a row reads the pulse or the value there. */
  #point(e: PointerEvent): void {
    const l = this.#plot.layout;
    if (!l || !this.#loaded) return;
    const box = this.#plot.canvas.getBoundingClientRect();
    const p = { x: e.clientX - box.left, y: e.clientY - box.top };
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
      this.#read([name, ...r.lines.map((t): Item => [[t === "high" || t === "low" ? "level" : "", t]])]);
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

  /** Lights the next note in time (`dir` 1) or the one before, and has it said. */
  step(dir: 1 | -1): void {
    const l = this.#loaded;
    if (!l) return;
    const notes = [...this.#plot.laneNotes()].sort((x, y) => x.start - y.start || (x.end ?? x.start) - (y.end ?? y.start));
    if (notes.length === 0) return;
    const at = notes.findIndex((a) => a.id === this.#lit);
    const next = at < 0 ? (dir > 0 ? 0 : notes.length - 1) : Math.max(0, Math.min(notes.length - 1, at + dir));
    this.#prober.pointer = null;
    this.#prober.hide();
    this.#light(notes[next].id, true);
  }

  /** Lights nothing and reads nothing. */
  clear(): void {
    this.#prober.pointer = null;
    this.#prober.hide();
    if (this.#plot.hoverRow !== null) {
      this.#plot.hoverRow = null;
      this.#plot.invalidate();
    }
    this.#light(null);
    this.#said.textContent = "";
  }

  /** Opens `for`'s view at the lit note or the inset's annotation; true if it did. */
  #openView(): boolean {
    const view = this.#view;
    const id = this.#lit ?? this.#annotation?.id;
    if (!view || !id) return false;
    void view.reveal(id);
    return true;
  }

  #clicked(e: MouseEvent): void {
    const touch = this.#touch;
    this.#touch = false;
    // A tap lights a note or reads a pulse; a tap on the note already lit opens at it.
    if (touch && (this.#lit === null || this.#lit !== this.#litAtTouch)) {
      e.preventDefault();
      return;
    }
    // A link goes where its address says, a new tab included; a view opens in place.
    if (this.#openView()) e.preventDefault();
  }

  #keyed(e: KeyboardEvent): void {
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      e.preventDefault();
      this.step(e.key === "ArrowRight" ? 1 : -1);
    } else if (e.key === "Escape" && this.#lit !== null) {
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

/** The `measure` readout as HTML: empty when the span says nothing, null for an unknown measure. */
export function measureText(what: string, channel: string, level: 0 | 1, ticks: Float64Array, info: RecordingInfo): string | null {
  const r = measureReading(what, level, ticks, info.timebase);
  if (r === undefined) return null;
  if (r === null) return "";
  return `<span class="what">${esc(channel)}</span> ${esc(r.value)} <span class="what">${esc(r.note)}</span>`;
}
