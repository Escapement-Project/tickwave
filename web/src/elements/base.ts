import { resolveAnnotations, type Resolved } from "../annotations.ts";
import { parseSeries } from "../attributes.ts";
import { onFaceChange, onFontsLoaded } from "../face.ts";
import { resolvePalette, type Palette } from "../palette.ts";
import { absolute, annotationsFor, infoFor, sourceFor } from "../registry.ts";
import type { RecordingInfo, Source } from "../source.ts";

export const SHARED_CSS = `
:host { display: block; color: var(--ink, CanvasText); font-family: var(--font-text, system-ui, sans-serif);
  font-size: 14px; line-height: 1.4; }
:host([hidden]) { display: none; }
.probe { display: none; }
button, select { font: inherit; font-size: 13px; line-height: 1.2; color: var(--ink, CanvasText); background: transparent;
  border: 1px solid var(--rule, GrayText); border-radius: 6px; padding: 3px 9px; min-height: 26px; cursor: pointer; }
select { padding-right: 4px; background: var(--raised, Canvas); }
button:hover, select:hover { background: var(--wash, color-mix(in srgb, CanvasText 12%, transparent)); }
button:disabled { opacity: .45; cursor: default; background: transparent; }
:focus-visible { outline: var(--focus-width, 2px) solid var(--ink, CanvasText); outline-offset: 2px; }
.figures { font-family: var(--font-figures, ui-monospace, monospace); font-stretch: var(--figures-stretch, normal);
  font-weight: var(--figures-weight, 500); font-variant-numeric: tabular-nums; font-size: 12px; }
`;

export interface Loaded {
  readonly src: URL;
  readonly source: Source;
  readonly info: RecordingInfo;
  readonly annotations: Resolved;
}

/** The recording at `src` and its annotations, shared with every element on the page. */
export async function loadRecording(src: string, annotations: string | null): Promise<Loaded> {
  const url = absolute(src);
  const [source, info, raw] = await Promise.all([
    sourceFor(url),
    infoFor(url),
    annotations ? annotationsFor(absolute(annotations)) : Promise.resolve([]),
  ]);
  return { src: url, source, info, annotations: resolveAnnotations(raw, info.timebase) };
}

export abstract class ChronoElement extends HTMLElement {
  protected readonly root: ShadowRoot;
  readonly #probe: HTMLSpanElement;
  protected palette: Palette | null = null;
  #unsubscribe: (() => void)[] = [];
  #queued = false;
  #reportedErrors = new Set<string>();

  constructor(css: string, html: string) {
    super();
    this.root = this.attachShadow({ mode: "open" });
    this.root.innerHTML = `<style>${SHARED_CSS}${css}</style><span class="probe" aria-hidden="true"></span>${html}`;
    this.#probe = this.root.querySelector(".probe")!;
  }

  connectedCallback(): void {
    this.#unsubscribe.push(
      onFaceChange(() => this.refreshPalette()),
      onFontsLoaded(() => this.fontsChanged()),
    );
    this.refreshPalette();
    this.queueUpdate();
  }

  disconnectedCallback(): void {
    for (const u of this.#unsubscribe.splice(0)) u();
  }

  attributeChangedCallback(name: string, old: string | null, value: string | null): void {
    if (old === value) return;
    if (name === "series") this.refreshPalette();
    this.queueUpdate();
  }

  /** Coalesces attribute changes made together into one update. */
  protected queueUpdate(): void {
    if (this.#queued) return;
    this.#queued = true;
    queueMicrotask(() => {
      this.#queued = false;
      if (this.isConnected) this.update();
    });
  }

  protected abstract update(): void;

  /** The series name, from this element or the one it borrows a recording from. */
  protected get series(): string | null {
    return parseSeries(this.getAttribute("series"));
  }

  protected refreshPalette(): void {
    if (!this.isConnected) return;
    this.palette = resolvePalette(this.#probe, this.series);
    this.paletteChanged();
  }

  protected abstract paletteChanged(): void;

  protected fontsChanged(): void {
    this.paletteChanged();
  }

  /** Reports a problem once as `chrono-error`; the element goes on with what it has. */
  protected fail(e: unknown): void {
    const message = e instanceof Error ? e.message : String(e);
    if (this.#reportedErrors.has(message)) return;
    this.#reportedErrors.add(message);
    this.dispatchEvent(new CustomEvent("chrono-error", { detail: { message, error: e }, bubbles: true, composed: true }));
  }

  protected emit(name: string, detail: unknown): void {
    this.dispatchEvent(new CustomEvent(name, { detail, bubbles: true, composed: true }));
  }
}
