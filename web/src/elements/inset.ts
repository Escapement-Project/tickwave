// <tickwave-inset>: one annotation's span, magnified, with its label, the annotations inside it as
// notes, and an optional `measure` readout. It sweeps in left to right once per appearance.

import { covers, type Annotation } from "../annotations.ts";
import { INSET_ATTRIBUTES, parseChannels, parsePad, parseScale, parseSweep } from "../attributes.ts";
import { reducedMotion } from "../face.ts";
import { measureReading } from "../readouts.ts";
import { channelIndices, Plot, ROLE_WEIGHT } from "../render/plot.ts";
import { isAbort, named, type RecordingInfo } from "../source.ts";
import { secondsValue, toTicks } from "../time.ts";
import { TickwaveElement, loadRecording, type Loaded } from "./base.ts";

const CSS = `
:host { max-width: 100%; }
figure { margin: 0; }
figcaption { margin: 0 4px 2px; font-size: 12px; font-weight: 600; line-height: 1.35; }
.text { margin: 0 4px 4px; font-size: 12px; line-height: 1.4; color: var(--ink-2, GrayText); }
.text[hidden] { display: none; }
.frame { position: relative; }
canvas { display: block; }
.foot { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: baseline; gap: 2px 14px;
  margin: 2px 4px 0; font-size: 12px; color: var(--ink-2, GrayText); }
.foot:empty { display: none; }
.measure { color: var(--ink, CanvasText); }
.measure .what { font-family: var(--font-text, system-ui, sans-serif); color: var(--ink-2, GrayText); }
.key { display: flex; gap: 12px; margin-left: auto; }
.key i { display: inline-block; width: 14px; height: 2px; vertical-align: middle; margin-right: 5px; }
.key i.heavy { height: 3px; }
`;

const HTML = `<figure part="inset"><figcaption part="label"></figcaption><p class="text" part="text" hidden></p>
<div class="frame" part="frame"><canvas part="canvas" aria-hidden="true"></canvas></div>
<div class="foot" part="foot"><span class="measure figures" part="measure"></span><span class="key" part="key"></span></div>
<div class="notice" part="notice" role="status" hidden></div></figure>`;

/** The landing page's sweep: steady across the trace. */
const SWEEP_MS = 420;
/** Reopened this soon after closing, an inset shows already drawn. */
const REAPPEAR_MS = 800;

export class TickwaveInset extends TickwaveElement {
  static observedAttributes = INSET_ATTRIBUTES;
  readonly #plot: Plot;
  readonly #caption: HTMLElement;
  readonly #measure: HTMLElement;
  readonly #key: HTMLElement;
  readonly #frame: HTMLElement;
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
    this.#plot.onStall = (ms) => this.waiting(ms);
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
      loadRecording(src, annotations).then(
        (l) => {
          if (this.#loadKey !== key) return;
          this.#loaded = l;
          for (const e of l.annotations.errors) this.fail(named("TickwaveSidecarError", e));
          this.#show();
        },
        (e) => {
          // A load the inset has moved on from says nothing about the one it shows now.
          if (this.#loadKey === key) this.fail(e);
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
    if (id && !a) this.fail(new Error(`no annotation ${id}`));
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
    this.#layout();
    this.#renderMeasure();
    this.#renderKey();
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
    void this.#plot.whenSettled().then(() => {
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

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

/** The `measure` readout as HTML: empty when the span says nothing, null for an unknown measure. */
export function measureText(what: string, channel: string, level: 0 | 1, ticks: Float64Array, info: RecordingInfo): string | null {
  const r = measureReading(what, level, ticks, info.timebase);
  if (r === undefined) return null;
  if (r === null) return "";
  return `<span class="what">${esc(channel)}</span> ${esc(r.value)} <span class="what">${esc(r.note)}</span>`;
}
