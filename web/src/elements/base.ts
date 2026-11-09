import { indexAnnotations, type Annotation, type Resolved } from "../annotations.ts";
import { parseSeries } from "../attributes.ts";
import { onFaceChange, onFontsLoaded } from "../face.ts";
import { resolvePalette, type Palette } from "../palette.ts";
import { absolute, annotationsFor, bundledFor, infoFor, sourceFor } from "../registry.ts";
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
.notice { margin: 4px 0 0; padding: 1px 8px; font-size: 12px; line-height: 1.4; color: var(--attention-ink, CanvasText);
  border-left: 2px solid var(--attention-ink, CanvasText); }
.notice[hidden] { display: none; }
.figures { font-family: var(--font-figures, ui-monospace, monospace); font-stretch: var(--figures-stretch, normal);
  font-weight: var(--figures-weight, 500); font-variant-numeric: tabular-nums; font-size: 12px; }
`;

export interface Loaded {
  readonly src: URL;
  readonly source: Source;
  readonly info: RecordingInfo;
  readonly annotations: Resolved;
}

/**
 * The recording at `src` and its annotations, shared with every element on the page. Without an
 * `annotations` URL, a bundled sidecar serves. A sidecar's problems are in `annotations.errors`;
 * the recording still draws.
 */
export async function loadRecording(src: string, annotations: string | null): Promise<Loaded> {
  const url = absolute(src);
  const [source, info] = await Promise.all([sourceFor(url), infoFor(url)]);
  let list: Annotation[] = [];
  const errors: string[] = [];
  try {
    list = (annotations ? await annotationsFor(absolute(annotations), url) : await bundledFor(url)) ?? [];
  } catch (e) {
    errors.push(e instanceof Error ? e.message : String(e));
  }
  return { src: url, source, info, annotations: indexAnnotations(list, errors) };
}

export abstract class TickwaveElement extends HTMLElement {
  protected readonly root: ShadowRoot;
  readonly #probe: HTMLSpanElement;
  protected palette: Palette | null = null;
  #unsubscribe: (() => void)[] = [];
  #queued = false;
  #reportedErrors = new Set<string>();
  #waiting: string | null = null;

  constructor(css: string, html: string) {
    super();
    this.root = this.attachShadow({ mode: "open" });
    this.root.innerHTML = `<style>${SHARED_CSS}${css}</style><span class="probe" aria-hidden="true"></span>${html}`;
    this.#probe = this.root.querySelector(".probe")!;
  }

  connectedCallback(): void {
    if (this.drawsWithPalette) {
      this.#unsubscribe.push(
        onFaceChange(() => this.refreshPalette()),
        onFontsLoaded(() => this.fontsChanged()),
      );
    }
    this.refreshPalette();
    this.queueUpdate();
  }

  /** Whether the element draws in the page's colours. One that only holds text, as a reference
   * does, reads none of them: a page of fifty references costs nothing on a face change. */
  protected get drawsWithPalette(): boolean {
    return true;
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
    if (!this.isConnected || !this.drawsWithPalette) return;
    this.palette = resolvePalette(this.#probe, this.series);
    this.paletteChanged();
  }

  protected abstract paletteChanged(): void;

  protected fontsChanged(): void {
    this.paletteChanged();
  }

  /**
   * Reports a problem once: as `tickwave-error`, in the console, and in a short notice in the
   * element, so a page's author sees it. The element goes on with what it has.
   */
  protected fail(e: unknown): void {
    const message = e instanceof Error ? e.message : String(e);
    if (this.#reportedErrors.has(message)) return;
    this.#reportedErrors.add(message);
    console.warn(`<${this.localName}${this.id ? ` id="${this.id}"` : ""}>: ${message}`);
    this.dispatchEvent(new CustomEvent("tickwave-error", { detail: { message, error: e }, bubbles: true, composed: true }));
    this.#showNotice();
  }

  /** Forgets the problems reported so far: a notice is about the recording or sidecar it came
   * from, and a new one starts without it. */
  protected clearProblems(): void {
    this.#reportedErrors.clear();
    this.#showNotice();
  }

  /** Forgets one problem, once what it was about has gone. */
  protected forgetProblem(message: string): void {
    if (this.#reportedErrors.delete(message)) this.#showNotice();
  }

  /** Says a read has run `ms` and is still going, or with null that none is. A problem comes first. */
  protected waiting(ms: number | null): void {
    this.#waiting = ms === null ? null : `Still loading after ${ms / 1000} s.`;
    this.#showNotice();
  }

  #showNotice(): void {
    const notice = this.root.querySelector(".notice") as HTMLElement | null;
    if (!notice) return;
    const problems = [...this.#reportedErrors].flatMap((m) => m.split("\n")).filter(Boolean);
    notice.hidden = problems.length === 0 && this.#waiting === null;
    notice.textContent =
      problems.length === 0
        ? (this.#waiting ?? "")
        : problems.length === 1
          ? problems[0]
          : `${problems[0]} (and ${problems.length - 1} more problems; see the console)`;
  }

  protected emit(name: string, detail: unknown): void {
    this.dispatchEvent(new CustomEvent(name, { detail, bubbles: true, composed: true }));
  }
}
