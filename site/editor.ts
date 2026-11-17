// The Open page's annotation editor. It edits the hand-written file of a recording's sidecar in
// place through the Rust core (convert-wasm's editAnnotations), which keeps the file's comments,
// order and layout. A change is kept only once the sidecar checks, as `tickwave annotations check`
// checks it, so the text in hand always does. Saving writes the file back where `tickwave open`
// serves it, or downloads it.

import { readSidecar, type Annotation, type RecordingInfo, type Role, type TickwaveView } from "tickwave";
import type { Edit, Fields, FromWorker, HandAnnotation, ToWorker } from "../web/src/convert/protocol.ts";
import { formatInterval } from "../web/src/readouts.ts";
import { idFrom, timeText } from "./editor-fields.ts";

/** Where a recording's sidecar comes from, and where it goes. */
export interface Sidecar {
  /** The recording's address, as the view has it. */
  src: string;
  /** The recording's file name: `recording` in a new sidecar, and the name of one to download. */
  recording: string;
  /** The sidecar's address, or null when the recording came without one. */
  annotations: string | null;
  /** The hand-written file of a sidecar opened from this computer. */
  file: File | null;
  /** Where `tickwave open` takes the file back, or null: the page downloads it. */
  save: string | null;
}

export interface Editor {
  /** Whether there are changes not yet saved or downloaded. */
  readonly unsaved: boolean;
  /** Puts the editor away for another recording. */
  close(): void;
}

const byId = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

/** The worker that edits: the converter's, kept for edits only. One request at a time. */
class EditWorker {
  #worker: Worker | null = null;
  #queue: Promise<unknown> = Promise.resolve();

  run(text: string, edit: Edit | null): Promise<{ text: string; hand: HandAnnotation[] }> {
    const next = this.#queue.then(
      () =>
        new Promise<{ text: string; hand: HandAnnotation[] }>((resolve, reject) => {
          this.#worker ??= new Worker(new URL("./tickwave-convert-worker.js", import.meta.url), { type: "module", name: "tickwave editor" });
          const w = this.#worker;
          w.onmessage = (e: MessageEvent<FromWorker>) => {
            const m = e.data;
            if (m.op === "edited") resolve({ text: m.text, hand: m.hand });
            else if (m.op === "failed") reject(new Error(m.message));
          };
          w.onerror = (e) => {
            e.preventDefault();
            this.#worker?.terminate();
            this.#worker = null;
            reject(new Error(e.message || "the editor's worker failed to start"));
          };
          w.postMessage({ op: "edit", text, edit } satisfies ToWorker);
        }),
    );
    this.#queue = next.catch(() => undefined);
    return next;
  }

  end(): void {
    this.#worker?.terminate();
    this.#worker = null;
  }
}

type Current = { kind: "new"; ref: string | null } | { kind: "hand"; index: number } | { kind: "generated"; id: string } | null;

/** The editor for the recording `info` shown in `view`: its Edit button opens it. */
export function attachEditor(view: TickwaveView, info: RecordingInfo, sidecar: Sidecar): Editor {
  const toggle = byId<HTMLButtonElement>("edit");
  const state = byId("edit-state");
  const panel = byId("editor");
  const problem = byId("editor-problem");
  const saveButton = byId<HTMLButtonElement>("sidecar-save");
  const nameLine = byId("sidecar-name");
  const form = byId<HTMLFormElement>("note");
  const title = byId("note-title");
  const about = byId("note-about");
  const noteProblem = byId("note-problem");
  const list = byId("notes");
  const f = {
    label: byId<HTMLInputElement>("note-label"),
    id: byId<HTMLInputElement>("note-id"),
    kind: byId<HTMLInputElement>("note-kind"),
    start: byId<HTMLInputElement>("note-start"),
    end: byId<HTMLInputElement>("note-end"),
    text: byId<HTMLTextAreaElement>("note-text"),
    paint: byId<HTMLInputElement>("note-paint"),
    what: byId<HTMLSelectElement>("note-what"),
    measure: byId<HTMLSelectElement>("note-measure"),
  };
  const channelBox = byId("note-channels");
  const apply = byId<HTMLButtonElement>("note-apply");
  const del = byId<HTMLButtonElement>("note-delete");
  const name = byId<HTMLButtonElement>("note-name");
  const tb = info.timebase;
  const worker = new EditWorker();
  const abort = new AbortController();
  const on = <K extends keyof HTMLElementEventMap>(el: EventTarget, type: K, f: (e: HTMLElementEventMap[K]) => void) =>
    el.addEventListener(type, f as EventListener, { signal: abort.signal });

  /** The hand-written file as edited, and as last saved, downloaded or read. */
  let text: string | null = null;
  let saved: string | null = null;
  let etag: string | null = null;
  let hand: HandAnnotation[] = [];
  let resolved: Annotation[] = [];
  /** The roles the sidecar declares, for the paint field's list. */
  let roles: readonly Role[] = [];
  let current: Current = null;
  /** The id field follows the label until it is typed in. */
  let idTyped = false;
  let starting: Promise<boolean> | null = null;
  /** While the sidecar is read, the annotation last named in the view: the editor opens on it. */
  let reading = false;
  let named: string | null = null;

  const fileName = sidecar.file?.name ?? (sidecar.save ?? sidecar.annotations)?.split("/").pop()?.split("?")[0] ?? null;
  const ownName = fileName ? decodeURIComponent(fileName) : `${sidecar.recording.replace(/\.tickwave$/i, "")}.annotations.toml`;
  const swap = (button: HTMLElement, second: boolean) => {
    const [a, b] = button.querySelectorAll(".swap > span");
    a.classList.toggle("off", second);
    b.classList.toggle("off", !second);
  };
  const say = (box: HTMLElement, lead: string | null, items: string[] = []) => {
    box.replaceChildren();
    box.hidden = lead === null;
    if (lead === null) return;
    box.append(Object.assign(document.createElement("p"), { textContent: lead }));
    if (items.length > 0) {
      const ul = document.createElement("ul");
      for (const i of items) ul.append(Object.assign(document.createElement("li"), { textContent: i }));
      box.append(ul);
    }
  };
  const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
  const showState = () => {
    state.textContent = text === null ? "" : text === saved ? "" : "Changes not saved";
  };
  const warnOnLeave = (e: BeforeUnloadEvent) => {
    if (text !== null && text !== saved) e.preventDefault();
  };
  on(window, "beforeunload" as keyof HTMLElementEventMap, warnOnLeave as (e: Event) => void);

  saveButton.textContent = sidecar.save ? "Save the sidecar" : "Download the sidecar";
  nameLine.textContent = sidecar.save ? `to ${ownName}, beside the recording` : `as ${ownName}`;

  /** Reads the file to edit, or starts one. */
  async function start(): Promise<boolean> {
    state.textContent = "Reading the sidecar";
    reading = true;
    try {
      let from: string | null = null;
      if (sidecar.save && sidecar.annotations) {
        const r = await fetch(sidecar.save, { cache: "no-store" });
        if (!r.ok) throw new Error(`${ownName} didn't arrive: the server answered ${r.status}`);
        etag = r.headers.get("ETag");
        from = await r.text();
      } else if (sidecar.annotations) {
        const r = await fetch(sidecar.annotations);
        if (!r.ok) throw new Error(`${ownName} didn't arrive: the server answered ${r.status}`);
        from = await r.text();
      } else if (sidecar.file) from = await sidecar.file.text();
      if (from === null) {
        if (!info.contentId) throw new Error("the recording states no content id for a sidecar to pin");
        from = (await worker.run("", { op: "start", recording: sidecar.recording, recording_id: info.contentId })).text;
      }
      const read = await worker.run(from, null);
      const whole = await readSidecar(sidecar.src, read.text, sidecar.annotations);
      [resolved, roles] = [whole.annotations, whole.roles];
      text = read.text;
      // A new file is a change until it is saved; one read from its place is not.
      saved = sidecar.annotations || sidecar.file ? read.text : null;
      hand = read.hand;
    } catch (e) {
      // Said beside the button: the panel stays closed.
      state.textContent = `The sidecar can't be edited here: ${message(e)}`;
      reading = false;
      named = null;
      return false;
    }
    reading = false;
    view.annotationsText = text;
    edit(null);
    if (named !== null) choose(named);
    named = null;
    showState();
    return true;
  }

  /** The editor on annotation `id`, where the sidecar holds it. */
  function choose(id: string): void {
    const a = resolved.find((x) => x.id === id);
    if (!a) return;
    edit(a.source === null ? { kind: "hand", index: hand.findIndex((h) => h.id === a.id) } : { kind: "generated", id: a.id });
  }

  /** Applies `edit`, keeping it only if the sidecar then checks. */
  async function change(edit: Edit): Promise<boolean> {
    if (text === null) return false;
    say(noteProblem, null);
    let next: { text: string; hand: HandAnnotation[] };
    let list: Annotation[];
    let declared: readonly Role[];
    try {
      next = await worker.run(text, edit);
    } catch (e) {
      say(noteProblem, `That change can't be made: ${message(e)}`);
      return false;
    }
    try {
      const whole = await readSidecar(sidecar.src, next.text, sidecar.annotations);
      [list, declared] = [whole.annotations, whole.roles];
    } catch (e) {
      say(noteProblem, "The sidecar wouldn't check with that change:", message(e).split("\n").filter(Boolean));
      return false;
    }
    text = next.text;
    hand = next.hand;
    resolved = list;
    roles = declared;
    view.annotationsText = text;
    renderList();
    showState();
    return true;
  }

  /** Each annotation, in sidecar order: the hand-written file's, then each generator's. */
  function renderList(): void {
    const handIds = hand.map((h) => h.id);
    const items = resolved.map((a) => {
      const li = document.createElement("li");
      const index = a.source === null ? handIds.indexOf(a.id) : -1;
      const pick = Object.assign(document.createElement("button"), { type: "button", className: "pick" });
      const isCurrent = (current?.kind === "hand" && current.index === index && index >= 0) || (current?.kind === "generated" && current.id === a.id);
      pick.setAttribute("aria-current", String(isCurrent));
      const when = a.end === null ? formatInterval(a.start, tb) : `${formatInterval(a.start, tb)} to ${formatInterval(a.end, tb)}`;
      pick.append(
        Object.assign(document.createElement("span"), { className: "what", textContent: [a.id, a.kind, a.label].filter(Boolean).join(" · ") }),
        Object.assign(document.createElement("span"), { className: "when", textContent: when }),
      );
      const named = a.source === null ? null : resolved.find((x) => x.ref === a.id);
      const from = a.source === null ? (a.ref ? `names ${a.ref}` : null) : `from ${a.source}${named ? `, named ${named.id}` : ""}`;
      if (from) pick.append(Object.assign(document.createElement("span"), { className: "from", textContent: from }));
      pick.addEventListener("click", () => {
        void view.show(a.id);
        if (a.source === null) edit({ kind: "hand", index });
        else edit({ kind: "generated", id: a.id });
      });
      li.append(pick);
      if (index >= 0) {
        for (const [glyph, by, label] of [
          ["↑", -1, "Move up"],
          ["↓", 1, "Move down"],
        ] as const) {
          const b = Object.assign(document.createElement("button"), { type: "button", className: "move", textContent: glyph });
          b.setAttribute("aria-label", `${label}: ${a.id}`);
          b.disabled = index + by < 0 || index + by >= hand.length;
          b.addEventListener("click", async () => {
            if (await change({ op: "move", from: index, to: index + by })) {
              if (current?.kind === "hand" && current.index === index) current = { kind: "hand", index: index + by };
              renderList();
              list.querySelectorAll<HTMLButtonElement>(".move")[2 * (index + by) + (by < 0 ? 0 : 1)]?.focus();
            }
          });
          li.append(b);
        }
      }
      return li;
    });
    list.replaceChildren(...items);
    const kinds = byId("note-kinds");
    kinds.replaceChildren(...[...new Set(resolved.map((a) => a.kind).filter((k): k is string => !!k))].map((k) => Object.assign(document.createElement("option"), { value: k })));
    // The roles the sidecar declares, in its order. A paint must name one: the check refuses any
    // other, as it refuses any change that breaks a rule.
    byId("note-paints").replaceChildren(...roles.map((r) => Object.assign(document.createElement("option"), { value: r.name, label: r.label })));
  }

  /** The recording's channels as ticks, and its digital ones as what a measure can read. */
  channelBox.replaceChildren(
    ...info.channels.map((c) => {
      const label = document.createElement("label");
      label.append(Object.assign(document.createElement("input"), { type: "checkbox", value: c.name }), c.name);
      return label;
    }),
  );
  f.measure.replaceChildren(...info.channels.filter((c) => c.kind === "digital").map((c) => Object.assign(document.createElement("option"), { value: c.name, textContent: c.name })));
  const ticks = () => [...channelBox.querySelectorAll<HTMLInputElement>("input")];

  /** The time field's value for the file: ticks when it is a whole number, else the text. */
  const time = (v: string): string | number | undefined => {
    const t = v.trim();
    if (t === "") return undefined;
    return /^\d+$/.test(t) ? Number(t) : t;
  };
  const asText = (v: unknown) => (v === undefined || v === null ? "" : String(v));

  /** The form on `what`: a new annotation, one of the hand-written file's, a generator's (to read
   * and to name), or none. It keeps its height in every case, so nothing under it moves. */
  function edit(what: Current): void {
    current = what;
    say(noteProblem, null);
    idTyped = what?.kind !== "new";
    const generated = what?.kind === "generated" ? resolved.find((a) => a.id === what.id) : undefined;
    const raw: HandAnnotation | undefined = what?.kind === "hand" ? hand[what.index] : undefined;
    const ref = what?.kind === "new" ? what.ref : (raw?.ref ?? null);
    /** What an alias takes from the annotation it names, or what a generator wrote. */
    const shown = generated ?? (ref ? resolved.find((a) => a.id === ref) : undefined);
    const editable = what !== null && what.kind !== "generated";
    for (const el of [f.label, f.id, f.kind, f.text, f.paint, f.what, f.measure]) el.disabled = !editable;
    for (const el of [f.start, f.end, byId<HTMLButtonElement>("note-take"), ...ticks()]) el.disabled = !editable || ref !== null;
    apply.disabled = !editable;
    swap(apply, what?.kind === "hand");
    del.disabled = what?.kind !== "hand";
    name.disabled = what?.kind !== "generated";
    renderList();

    const field = (k: "label" | "id" | "kind" | "text" | "paint") => asText(generated ? generated[k] : raw?.[k]);
    f.label.value = field("label");
    f.id.value = field("id");
    f.kind.value = field("kind");
    f.text.value = field("text");
    f.paint.value = field("paint");
    const measure = generated?.measure ?? raw?.measure;
    f.what.value = asText(measure?.what);
    if (measure) f.measure.value = measure.channel;
    f.start.value = shown ? String(timeText(shown.start, tb)) : asText(raw?.start);
    f.end.value = shown ? (shown.end === null ? "" : String(timeText(shown.end, tb))) : asText(raw?.end);
    const chosen = new Set(shown ? (shown.channels ?? []) : (raw?.channels ?? []));
    for (const t of ticks()) t.checked = chosen.has(t.value);
    // An alias takes any field it leaves empty from the annotation it names: shown, not written.
    const inherited = ref ? shown : undefined;
    for (const k of ["label", "kind"] as const) f[k].placeholder = inherited?.[k] ?? "";
    f.text.placeholder = inherited?.text ?? "";

    const note = what === null ? "none" : generated ? "generated" : ref ? "alias" : "own";
    for (const s of about.querySelectorAll<HTMLElement>("[data-about]")) s.classList.toggle("off", s.dataset.about !== note);
    if (what === null) {
      title.textContent = "No annotation chosen";
      return;
    }
    if (generated) {
      title.textContent = generated.id;
      return;
    }
    title.textContent = what.kind === "new" ? (ref ? `A name for ${ref}` : "New annotation") : asText(raw?.id);
    if (what.kind === "new") {
      if (!ref && !takeSpan()) {
        edit(null);
        say(problem, "Select a span first, by dragging along the time axis, or place cursor A with a click.");
        return;
      }
      f.id.value = idFrom(f.label.value || f.label.placeholder || f.kind.value, new Set(resolved.map((a) => a.id)));
    }
    say(problem, null);
    f.label.focus();
  }

  /** The selection's span, or a point at cursor A, into the time fields. False when there is neither. */
  function takeSpan(): boolean {
    const s = view.selection;
    const a = view.cursors.a;
    if (s) {
      f.start.value = String(timeText(s.start, tb));
      f.end.value = String(timeText(s.end, tb));
    } else if (a !== null) {
      f.start.value = String(timeText(a, tb));
      f.end.value = "";
    } else return false;
    return true;
  }

  /** The form as the file's fields. */
  function fields(): Fields {
    const out: Fields = { id: f.id.value.trim() };
    const ref = current?.kind === "new" ? current.ref : current?.kind === "hand" ? (hand[current.index]?.ref ?? null) : null;
    if (!ref) {
      const start = time(f.start.value);
      const end = time(f.end.value);
      if (start !== undefined) out.start = start;
      if (end !== undefined) out.end = end;
      const channels = ticks().filter((t) => t.checked).map((t) => t.value);
      if (channels.length > 0) out.channels = channels;
    }
    for (const k of ["label", "kind", "text", "paint"] as const) {
      const v = (k === "text" ? f.text.value : f[k].value).trim();
      if (v) out[k] = v;
    }
    if (f.what.value && f.measure.value) out.measure = { channel: f.measure.value, what: f.what.value };
    if (ref) out.ref = ref;
    return out;
  }

  on(f.label, "input", () => {
    if (current?.kind === "new" && !idTyped) f.id.value = idFrom(f.label.value || f.label.placeholder || f.kind.value, new Set(resolved.map((a) => a.id)));
  });
  on(f.id, "input", () => (idTyped = true));
  on(byId("note-take"), "click", () => {
    if (!takeSpan()) say(noteProblem, "There's no selection or cursor A to take: drag along the time axis, or click to place cursor A.");
  });
  on(form, "submit", async (e) => {
    e.preventDefault();
    const c = current;
    if (c === null || c.kind === "generated") return;
    if (c.kind === "new") {
      const index = hand.length;
      if (await change({ op: "insert", index, fields: fields() })) edit({ kind: "hand", index });
    } else if (await change({ op: "replace", index: c.index, fields: fields() })) edit({ kind: "hand", index: c.index });
  });
  on(del, "click", async () => {
    if (current?.kind !== "hand") return;
    if (await change({ op: "remove", index: current.index })) edit(null);
  });
  on(name, "click", () => {
    if (current?.kind === "generated") edit({ kind: "new", ref: current.id });
  });
  on(byId("note-new"), "click", () => edit({ kind: "new", ref: null }));
  on(view, "tickwave-annotation" as keyof HTMLElementEventMap, ((e: CustomEvent<{ id: string }>) => {
    if (reading) named = e.detail.id;
    else if (!panel.hidden) choose(e.detail.id);
  }) as (e: Event) => void);

  on(saveButton, "click", async () => {
    if (text === null) return;
    say(problem, null);
    const body = text;
    if (!sidecar.save) {
      const a = Object.assign(document.createElement("a"), { href: URL.createObjectURL(new Blob([body], { type: "application/toml" })), download: ownName });
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 0);
      saved = body;
      showState();
      return;
    }
    let r: Response;
    try {
      r = await fetch(sidecar.save, {
        method: "PUT",
        headers: { "Content-Type": "application/toml", ...(etag ? { "If-Match": etag } : { "If-None-Match": "*" }) },
        body,
      });
    } catch (e) {
      say(problem, `${ownName} wasn't saved: the server gave no answer, so tickwave open may have stopped (${message(e)}).`);
      return;
    }
    if (r.ok) {
      etag = r.headers.get("ETag");
      saved = body;
      showState();
      if (!sidecar.annotations) {
        // The file is there now: the address names it, so the page opens with it again.
        sidecar.annotations = sidecar.save;
        const q = new URLSearchParams(location.search);
        q.set("annotations", sidecar.save);
        history.replaceState(null, "", `?${q.toString().replace(/&?local=(?=&|$)/, "")}&local`);
      }
      return;
    }
    const why = (await r.text()).trim();
    if (r.status === 412) say(problem, `${ownName} changed on disk since this page read it, so it wasn't saved over. Download your version to compare, or reload the page to edit the one on disk.`);
    else if (r.status === 400) say(problem, `${ownName} wasn't saved: it doesn't check.`, why.split("\n").filter(Boolean));
    else say(problem, `${ownName} wasn't saved: the server answered ${r.status}${why ? `: ${why}` : ""}.`);
  });

  on(toggle, "click", async () => {
    const opening = panel.hidden === true;
    if (opening) {
      starting ??= start();
      if (!(await starting)) {
        starting = null;
        return;
      }
    }
    panel.hidden = !opening;
    toggle.setAttribute("aria-expanded", String(opening));
    swap(toggle, opening);
  });

  toggle.parentElement!.hidden = false;
  panel.hidden = true;
  toggle.setAttribute("aria-expanded", "false");
  swap(toggle, false);
  list.replaceChildren();
  say(problem, null);
  state.textContent = "";

  return {
    get unsaved() {
      return text !== null && text !== saved;
    },
    close() {
      abort.abort();
      worker.end();
      panel.hidden = true;
      state.textContent = "";
    },
  };
}
