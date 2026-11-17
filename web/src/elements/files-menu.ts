// A view's Annotations button (the `download` attribute) and the list it opens over the page: a
// link to download each file of the sidecar, the hand-written file first, then each file it
// includes, in its order, each under its own name.

import { annotationsStarted } from "../registry.ts";
import { includesOf } from "../wasm/client.ts";

/** A view's sidecar files: served beside its hand-written file, or registered with a local
 * recording, its one `.toml` among them. */
export type SidecarFiles =
  | { readonly kind: "served"; readonly toml: URL; readonly recording: URL }
  | { readonly kind: "local"; readonly files: readonly File[] };

interface Link {
  readonly name: string;
  readonly href: string;
}

/** The files the hand-written one includes, and why they can't be listed where they can't. */
interface Rest {
  readonly links: Link[];
  readonly problem: string | null;
}

const blobs = new WeakMap<File, string>();

/** A local file's URL to download it from, made once, kept as long as the page. */
function blobOf(file: File): string {
  let url = blobs.get(file);
  if (!url) blobs.set(file, (url = URL.createObjectURL(file)));
  return url;
}

const fileName = (url: URL) => decodeURIComponent(url.pathname.split("/").pop() ?? "");

function same(a: SidecarFiles | null, b: SidecarFiles | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind === "served" && b.kind === "served") return a.toml.href === b.toml.href && a.recording.href === b.recording.href;
  return a.kind === "local" && b.kind === "local" && a.files === b.files;
}

function toml(from: SidecarFiles): Link {
  if (from.kind === "served") return { name: fileName(from.toml), href: from.toml.href };
  const hand = from.files.find((f) => f.name.endsWith(".toml"))!;
  return { name: hand.name, href: blobOf(hand) };
}

/** The files past the hand-written one. A served sidecar's names come from the view's read of it
 * where it has made one; else, and where that read failed, its hand-written file is read for them. */
async function readRest(from: SidecarFiles): Promise<Rest> {
  try {
    if (from.kind === "served") {
      const started = annotationsStarted(from.toml, from.recording);
      const known = started && (await started.then((s) => s.files ?? null, () => null));
      const names = known ?? (await includesOf({ url: from.toml }));
      return {
        links: names.map((n) => {
          const url = new URL(n, from.toml);
          return { name: fileName(url), href: url.href };
        }),
        problem: null,
      };
    }
    const hand = from.files.find((f) => f.name.endsWith(".toml"))!;
    const names = await includesOf({ text: await hand.text(), where: hand.name });
    const links = names.flatMap((n) => {
      const file = from.files.find((f) => f.name === n);
      return file ? [{ name: file.name, href: blobOf(file) }] : [];
    });
    return { links, problem: null };
  } catch (e) {
    return { links: [], problem: `The files it includes can't be listed: ${e instanceof Error ? e.message : String(e)}` };
  }
}

export class FilesMenu {
  readonly #button: HTMLButtonElement;
  readonly #list: HTMLElement;
  #from: SidecarFiles | null = null;
  #title = "";
  /** The files past the hand-written one, read the first time the list opens. */
  #rest: Promise<Rest> | null = null;
  /** The rest is in the list. */
  #listed = false;

  constructor(button: HTMLButtonElement, list: HTMLElement) {
    [this.#button, this.#list] = [button, list];
    button.addEventListener("click", () => (this.#isOpen ? this.close(false) : this.#open()));
    for (const e of [button, list]) {
      e.addEventListener("keydown", (k) => {
        if (k.key !== "Escape" || !this.#isOpen) return;
        k.stopPropagation();
        this.close(true);
      });
      // Focus leaving for anywhere but the button or the list closes it: a press elsewhere too,
      // and the list takes the focus itself where a press inside it meets no link.
      e.addEventListener("focusout", (f) => {
        const to = f.relatedTarget;
        if (to !== button && !(to instanceof Node && list.contains(to))) this.close(false);
      });
    }
  }

  get #isOpen(): boolean {
    return !this.#list.hidden;
  }

  /** Shows the button for `from`'s files, of the recording `title` names, or hides it where
   * `from` is null. Other files close the list and are read afresh. */
  set(from: SidecarFiles | null, title: string): void {
    this.#button.hidden = from === null;
    if (!same(from, this.#from)) {
      this.close(false);
      [this.#from, this.#rest, this.#listed] = [from, null, false];
      this.#list.replaceChildren();
    }
    if (from === null) return;
    this.#title = title;
    this.#button.setAttribute("aria-label", `Download the annotations of ${title}`);
    this.#list.setAttribute("aria-label", `The annotations of ${title}`);
    for (const a of this.#list.querySelectorAll("a")) a.setAttribute("aria-label", this.#label(a.download));
  }

  /** Closes the list, and gives the button the focus where `focus`. */
  close(focus: boolean): void {
    if (!this.#isOpen) return;
    this.#list.hidden = true;
    this.#button.setAttribute("aria-expanded", "false");
    if (focus) this.#button.focus();
  }

  #label(name: string): string {
    return `Download ${name}, from the annotations of ${this.#title}`;
  }

  #link({ name, href }: Link): HTMLAnchorElement {
    const a = document.createElement("a");
    a.setAttribute("part", "annotations-download-link");
    [a.href, a.download, a.textContent] = [href, name, name];
    a.setAttribute("aria-label", this.#label(name));
    return a;
  }

  #open(): void {
    const from = this.#from;
    if (!from) return;
    if (!this.#list.firstChild) this.#list.append(this.#link(toml(from)));
    this.#list.hidden = false;
    this.#button.setAttribute("aria-expanded", "true");
    this.#list.querySelector("a")!.focus();
    const rest = (this.#rest ??= readRest(from));
    // Added below the links already there, so none moves under the reader.
    void rest.then(({ links, problem }) => {
      if (this.#from !== from || this.#listed) return;
      this.#listed = true;
      this.#list.append(...links.map((l) => this.#link(l)));
      if (problem) {
        const note = document.createElement("p");
        note.className = "note";
        note.setAttribute("part", "annotations-download-note");
        note.textContent = problem;
        this.#list.append(note);
      }
    });
  }
}
