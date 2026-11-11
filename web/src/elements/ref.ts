// <tickwave-ref>: text that cites an annotation. Hover or focus opens a <tickwave-inset> in a popover
// beside the text, so nothing on the page moves. The arrow keys zoom and move it, and step through its
// notes; a click or Enter opens the `for` view there, or at the note the pointer or the keys have lit.

import { autoUpdate, computePosition, flip, inline, offset, shift } from "@floating-ui/dom";
import { REF_ATTRIBUTES } from "../attributes.ts";
import { absolute, annotationsFor, bundledFor, prefetch } from "../registry.ts";
import { TickwaveElement } from "./base.ts";
import type { TickwaveInset } from "./inset.ts";
import type { TickwaveView } from "./view.ts";

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
:host([for]) .pop { cursor: pointer; }
`;

/** A mouse that leaves the text or the popover has this long to reach the other. */
const GRACE_MS = 300;

export class TickwaveRef extends TickwaveElement {
  static observedAttributes = REF_ATTRIBUTES;
  readonly #pop: HTMLElement;
  #inset: TickwaveInset | null = null;
  #stopPosition: (() => void) | null = null;
  #intersect: IntersectionObserver;
  #prefetched = "";
  #closing = 0;
  /** The prefetched sidecar's annotations, which describe the reference. */
  #list: readonly { id: string; label: string | null; text: string | null }[] = [];

  constructor() {
    super(CSS, `<slot></slot><div class="pop" part="popover" popover="manual"></div>`);
    this.#pop = this.root.querySelector(".pop")!;
    this.#intersect = new IntersectionObserver((seen) => {
      if (seen.some((s) => s.isIntersecting)) this.#prefetch();
    });
    // The popover is inside the reference, so the pointer moving into it enters the reference again.
    this.addEventListener("pointerenter", () => {
      clearTimeout(this.#closing);
      this.#open();
    });
    // A finger's pointer leaves as it lifts, so only a mouse's leaving closes.
    this.addEventListener("pointerleave", (e) => {
      if (e.pointerType !== "mouse") return;
      clearTimeout(this.#closing);
      this.#closing = window.setTimeout(() => this.#close(), GRACE_MS);
    });
    this.addEventListener("focus", () => this.#open());
    this.addEventListener("blur", () => this.#close());
    // A tap in the popover that only lit a note has been answered there.
    this.addEventListener("click", (e) => {
      if (!e.defaultPrevented) void this.#go();
    });
    this.addEventListener("keydown", (e) => {
      // As a button answers: Enter, and Space, which would otherwise scroll the page.
      if (e.key === "Enter" || (e.key === " " && this.hasAttribute("for"))) {
        e.preventDefault();
        void this.#go();
      } else if (this.#inset && this.#pop.matches(":popover-open") && this.#inset.arrowKey(e)) {
        e.preventDefault();
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
  get #view(): TickwaveView | null {
    const id = this.getAttribute("for");
    const el = id ? document.getElementById(id) : null;
    return el?.localName === "tickwave-view" ? (el as TickwaveView) : null;
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
    // Another recording or sidecar: its annotations describe the reference once fetched.
    const key = `${this.#borrowed("src")}\n${this.#borrowed("annotations") ?? ""}`;
    if (this.#prefetched && key !== this.#prefetched) {
      this.#list = [];
      this.#prefetched = "";
      this.#prefetch();
    }
    this.#describe();
  }

  protected paletteChanged(): void {}

  protected override get drawsWithPalette(): boolean {
    return false;
  }

  #prefetch(): void {
    const src = this.#borrowed("src");
    if (!src) return;
    const annotations = this.#borrowed("annotations");
    const key = `${src}\n${annotations ?? ""}`;
    if (key === this.#prefetched) return;
    this.#prefetched = key;
    const recording = absolute(src);
    const sidecar = annotations ? absolute(annotations) : null;
    // A recording or sidecar the reference has moved off says nothing about the one it cites now.
    prefetch(recording, sidecar)
      .then(() => (sidecar ? annotationsFor(sidecar, recording) : bundledFor(recording)))
      .then((list) => {
        if (this.#prefetched !== key) return;
        this.#list = list ?? [];
        this.#describe();
      })
      .catch((e) => {
        if (this.#prefetched === key) this.fail(e);
      });
  }

  /** The inset is drawn, and a screen reader cannot reach into it: the annotation's label and text
   * describe the reference itself. */
  #describe(): void {
    const a = this.#list.find((x) => x.id === this.getAttribute("annotation"));
    const description = a ? [a.label, a.text].filter(Boolean).join(". ") : "";
    if (description) this.setAttribute("aria-description", description);
    else this.removeAttribute("aria-description");
  }

  #configure(inset: TickwaveInset): void {
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
      this.#inset = document.createElement("tickwave-inset") as TickwaveInset;
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
    clearTimeout(this.#closing);
    this.#inset?.clear();
    this.#stopPosition?.();
    this.#stopPosition = null;
    if (this.#pop.matches(":popover-open")) this.#pop.hidePopover();
  }

  /** Opens the `for` view at the inset's lit note, else at the annotation. */
  async #go(): Promise<void> {
    const view = this.#view;
    const a = (this.#pop.matches(":popover-open") ? this.#inset?.lit : null) ?? this.getAttribute("annotation");
    if (!view || !a) return;
    this.#close();
    await view.reveal(a);
  }
}
