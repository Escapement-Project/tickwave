// <chrono-ref>: text that cites an annotation. Hover or focus opens a <chrono-inset> in a popover
// beside the text, so nothing on the page moves; a click or Enter opens the `for` view there.

import { autoUpdate, computePosition, flip, inline, offset, shift } from "@floating-ui/dom";
import { REF_ATTRIBUTES } from "../attributes.ts";
import { reducedMotion } from "../face.ts";
import { absolute, prefetch } from "../registry.ts";
import { ChronoElement } from "./base.ts";
import type { ChronoInset } from "./inset.ts";
import type { ChronoView } from "./view.ts";

const CSS = `
:host { display: inline; font: inherit; line-height: inherit; color: inherit;
  text-decoration: underline dotted var(--ink-2, GrayText); text-decoration-thickness: 1.5px; text-underline-offset: .22em; }
:host(:hover), :host(:focus-visible) { text-decoration-style: solid; text-decoration-color: var(--ink, CanvasText); }
:host([for]) { cursor: pointer; }
:host(:focus-visible) { outline: var(--focus-width, 2px) solid var(--ink, CanvasText); outline-offset: 3px; border-radius: 2px; }
.pop { position: fixed; inset: auto; margin: 0; box-sizing: border-box; width: min(440px, calc(100vw - 16px));
  padding: 8px 6px 4px; color: var(--ink, CanvasText); background: var(--page, Canvas);
  border: 1px solid var(--hair, GrayText); border-radius: 10px; box-shadow: 0 2px 10px rgba(0, 0, 0, .14);
  text-decoration: none; cursor: default; overflow: visible; }
`;

export class ChronoRef extends ChronoElement {
  static observedAttributes = REF_ATTRIBUTES;
  readonly #pop: HTMLElement;
  #inset: ChronoInset | null = null;
  #stopPosition: (() => void) | null = null;
  #intersect: IntersectionObserver;
  #prefetched = "";

  constructor() {
    super(CSS, `<slot></slot><div class="pop" part="popover" popover="manual"></div>`);
    this.#pop = this.root.querySelector(".pop")!;
    this.#intersect = new IntersectionObserver((seen) => {
      if (seen.some((s) => s.isIntersecting)) this.#prefetch();
    });
    this.addEventListener("pointerenter", () => this.#open());
    // A finger's pointer leaves as it lifts, so only a mouse's leaving closes.
    this.addEventListener("pointerleave", (e) => {
      if (e.pointerType === "mouse") this.#close();
    });
    this.addEventListener("focus", () => this.#open());
    this.addEventListener("blur", () => this.#close());
    this.addEventListener("click", () => void this.#go());
    this.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void this.#go();
      }
    });
  }

  override connectedCallback(): void {
    super.connectedCallback();
    if (!this.hasAttribute("tabindex")) this.tabIndex = 0;
    this.#intersect.observe(this);
    document.addEventListener("keydown", this.#escape);
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.#intersect.disconnect();
    document.removeEventListener("keydown", this.#escape);
    this.#close();
  }

  #escape = (e: KeyboardEvent) => {
    if (e.key === "Escape") this.#close();
  };

  /** The view named by `for`, if any. */
  get #view(): ChronoView | null {
    const id = this.getAttribute("for");
    const el = id ? document.getElementById(id) : null;
    return el?.localName === "chrono-view" ? (el as ChronoView) : null;
  }

  /** src, annotations, channels and series: the ref's own, else its view's. */
  #borrowed(name: string): string | null {
    return this.getAttribute(name) ?? this.#view?.getAttribute(name) ?? null;
  }

  protected override get series(): string | null {
    return this.#borrowed("series");
  }

  protected update(): void {
    if (this.hasAttribute("for")) this.setAttribute("role", "button");
    else if (this.getAttribute("role") === "button") this.removeAttribute("role");
    if (this.#inset) this.#configure(this.#inset);
  }

  protected paletteChanged(): void {}

  #prefetch(): void {
    const src = this.#borrowed("src");
    if (!src) return;
    const annotations = this.#borrowed("annotations");
    const key = `${src}\n${annotations ?? ""}`;
    if (key === this.#prefetched) return;
    this.#prefetched = key;
    prefetch(absolute(src), annotations ? absolute(annotations) : null).catch((e) => this.fail(e));
  }

  #configure(inset: ChronoInset): void {
    for (const name of ["src", "annotations", "channels", "series"]) {
      const v = this.#borrowed(name);
      if (v === null) inset.removeAttribute(name);
      else if (inset.getAttribute(name) !== v) inset.setAttribute(name, v);
    }
    const a = this.getAttribute("annotation");
    if (a !== null && inset.getAttribute("annotation") !== a) inset.setAttribute("annotation", a);
  }

  #open(): void {
    if (!this.#borrowed("src") || this.#pop.matches(":popover-open")) return;
    if (!this.#inset) {
      this.#inset = document.createElement("chrono-inset") as ChronoInset;
      this.#configure(this.#inset);
      this.#pop.append(this.#inset);
    }
    this.#pop.showPopover();
    const place = () =>
      computePosition(this, this.#pop, {
        strategy: "fixed",
        placement: "bottom-start",
        middleware: [inline(), offset(6), flip({ padding: 8 }), shift({ padding: 8 })],
      }).then(({ x, y }) => {
        this.#pop.style.left = `${x}px`;
        this.#pop.style.top = `${y}px`;
      });
    this.#stopPosition = autoUpdate(this, this.#pop, place);
  }

  #close(): void {
    this.#stopPosition?.();
    this.#stopPosition = null;
    if (this.#pop.matches(":popover-open")) this.#pop.hidePopover();
  }

  /** Opens the `for` view at the annotation. */
  async #go(): Promise<void> {
    const view = this.#view;
    const a = this.getAttribute("annotation");
    if (!view || !a) return;
    this.#close();
    const shown = view.show(a);
    view.scrollIntoView({ block: "nearest", behavior: reducedMotion() ? "auto" : "smooth" });
    await shown;
    (view.shadowRoot?.querySelector(".plot") as HTMLElement | null)?.focus({ preventScroll: true });
  }
}
