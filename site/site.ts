// tickwave's site: the Open page, where a recording opens, or a capture converts and opens. Built by
// web/scripts/site.ts; "tickwave" is the tickwave.js beside this script. The project's face.js and
// header.js run the header.

import { previewMerge, recordingInfo, registerFile, type Channel, type ChannelShape, type MergedSource, type RecordingInfo, type TickwaveView } from "tickwave";
import { parseWindow } from "../web/src/attributes.ts";
import { formatBytes, formatInterval } from "../web/src/readouts.ts";
import { readingStep } from "../web/src/source.ts";
import { parseTime, toTicks } from "../web/src/time.ts";
import { githubAttachment } from "./attachment.ts";
import { attachEditor, type Editor } from "./editor.ts";
import { timeText } from "./editor-fields.ts";
import { DEFAULTS, type ChannelInfo, type Described, type FromWorker, type MergeInput, type MergeSettings, type Pass, type Settings, type SourceKind, type ToWorker } from "../web/src/convert/protocol.ts";

const byId = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

/** Says what went wrong in `box`: a lead sentence, and a list under it. */
function problems(box: HTMLElement): { say: (lead: string, items?: string[]) => void; clear: () => void } {
  return {
    say(lead, items = []) {
      box.replaceChildren();
      box.append(Object.assign(document.createElement("p"), { textContent: lead }));
      if (items.length > 0) {
        const ul = document.createElement("ul");
        for (const item of items) ul.append(Object.assign(document.createElement("li"), { textContent: item }));
        box.append(ul);
      }
      box.hidden = false;
    },
    clear() {
      box.hidden = true;
      box.replaceChildren();
    },
  };
}

/** A channel as the details list it: its name, an analog one's unit, and how it was moved, and
 * where it holds the source's data, where it states them. */
function channelFacts(c: Channel, tb: { num: number; den: number }, step: number): string {
  const notes = c.kind === "analog" ? ["analog", c.unit] : [];
  if (c.offset) {
    const why = c.offset.kind === "delay" ? "for a known delay" : `(${c.offset.kind})`;
    notes.push(`moved ${formatInterval(Math.abs(c.offset.ticks), tb, step)} ${c.offset.ticks > 0 ? "later" : "earlier"} ${why}`);
  }
  // A recording with a channel moved or cut states each channel's span of source data.
  if (c.kept) notes.push(`source data from ${formatInterval(c.kept.from, tb, step)} to ${formatInterval(c.kept.to, tb, step)}`);
  return notes.length > 0 ? `${c.name} (${notes.join(", ")})` : c.name;
}

/** A recording's facts as a definition list. */
function showFacts(facts: HTMLElement, info: RecordingInfo, name: string): void {
  const rows: [string, string][] = [
    ["Recording", info.title || name],
    ["Length", formatInterval(info.length, info.timebase, readingStep(info))],
    // A merged recording's sources tick at their own rates: the finest one's tick.
    ["One tick", formatInterval(readingStep(info), info.timebase, readingStep(info))],
    ["Channels", info.channels.map((c, i) => channelFacts(c, info.timebase, readingStep(info, [i]))).join(", ")],
  ];
  if (info.bytes !== undefined) rows.push(["Size", formatBytes(info.bytes)]);
  for (const source of info.sources ?? []) {
    rows.push(["From", [source.title, source.format, ...source.files].filter(Boolean).join(", ")]);
    // A merged recording's source keeps the metadata of the recording it came from.
    for (const [k, v] of Object.entries(source.meta ?? {})) rows.push([k, v]);
    if (source.rate === "inferred") rows.push(["Sample rate", "inferred from the edges"]);
    else if (source.rate === "given") rows.push(["Sample rate", "given when it was converted"]);
    if (source.converter) rows.push(["Converted by", source.converter]);
  }
  for (const [k, v] of Object.entries(info.meta ?? {})) rows.push([k, v]);
  if (info.contentId) rows.push(["Content id", info.contentId]);
  facts.replaceChildren(...rows.flatMap(([k, v]) => [Object.assign(document.createElement("dt"), { textContent: k }), Object.assign(document.createElement("dd"), { textContent: v })]));
}

/** The files of a dropped folder, read from its entries; or the dropped files themselves. */
async function dropped(data: DataTransfer): Promise<{ files: File[]; folder: string | null }> {
  const entries = [...data.items].map((i) => i.webkitGetAsEntry()).filter((e): e is FileSystemEntry => e !== null);
  const dir = entries.length === 1 && entries[0].isDirectory ? (entries[0] as FileSystemDirectoryEntry) : null;
  if (!dir) return { files: [...data.files], folder: null };
  const reader = dir.createReader();
  const all: FileSystemEntry[] = [];
  for (;;) {
    const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
    if (batch.length === 0) break;
    all.push(...batch);
  }
  const files = await Promise.all(
    all.filter((e) => e.isFile).map((e) => new Promise<File>((resolve, reject) => (e as FileSystemFileEntry).file(resolve, reject))),
  );
  return { files, folder: dir.name };
}

const count = (n: number) => n.toLocaleString("en-GB");

/** A recording, its sidecar's files, and a capture to convert, told apart by name. */
const RECORDING = /\.tickwave$/i;
const SIDECAR = /\.(toml|jsonl)$/i;
const CAPTURE = /\.(sr|vcd|bin)$/i;
const ZIP = /\.zip$/i;
/** The ends of the names the page opens: a zip's files are taken by them. */
const OPENED = [".tickwave", ".toml", ".jsonl", ".sr", ".vcd", ".bin"];

/** A capture's conversion: the recording, the codes found, how long it took, and the settings. */
interface Conversion {
  file: File;
  codes: [string, number | null][];
  seconds: number;
  settings: Settings;
}

/** The Open page: a recording by its address or from files, or a capture converted here and opened. */
function openPage(): void {
  const form = byId<HTMLFormElement>("by-url");
  const srcField = form.elements.namedItem("src") as HTMLInputElement;
  const sidecarField = form.elements.namedItem("annotations") as HTMLInputElement;
  const filesInput = byId<HTMLInputElement>("files");
  const folderInput = byId<HTMLInputElement>("folder");
  const drop = byId("drop");
  const opener = byId<HTMLDetailsElement>("opener");
  const settingsForm = byId<HTMLFormElement>("settings");
  const rows = byId("channels");
  const skippedBox = byId("skipped");
  const rateRow = byId("rate-row");
  const rateField = byId<HTMLInputElement>("rate");
  const running = byId("running");
  const bar = byId<HTMLProgressElement>("bar");
  const stage = byId("stage");
  const saving = byId("saving");
  const save = byId<HTMLButtonElement>("save");
  const reshape = byId<HTMLButtonElement>("reshape");
  const found = byId("found");
  const facts = byId("facts");
  const share = byId("share");
  const here = byId("view");
  const attached = byId("attached");
  const attachedLink = byId<HTMLAnchorElement>("attached-link");
  const attachedFile = byId<HTMLInputElement>("attached-file");
  const pick = byId("pick");
  const pickList = byId("pick-list");
  const adding = byId("adding");
  const addFiles = byId<HTMLInputElement>("add-files");
  const addFolder = byId<HTMLInputElement>("add-folder");
  const { say, clear } = problems(byId("problem"));
  /** Served by `tickwave open`, which names the recording by an address that lasts as long as it runs. */
  const local = new URLSearchParams(location.search).has("local");
  /** Where `tickwave open` takes the sidecar's hand-written file back. */
  const saveTo = local ? new URLSearchParams(location.search).get("save") : null;
  let editor: Editor | null = null;

  /** Counts the recordings asked for: one asked for later makes an earlier one's answer stale. */
  let opened = 0;
  let kind: SourceKind = "saleae";
  let files: File[] = [];
  /** The recording's name, without .tickwave: the export's folder, or the capture file's. */
  let stem = "capture";
  let channels: ChannelInfo[] = [];
  /** Each source channel's name in the recording onStage, as the last conversion named it. */
  let shownNames = new Map<string, string>();
  let recordingTimebase: { num: number; den: number } | null = null;
  /** The view of the recording converted here, which the table's moves and trims shape. */
  let shapedView: TickwaveView | null = null;
  let worker: Worker | null = null;
  /** The capture converted whole, every channel under its own name: what the view shapes, and
   * what Save keeps when nothing is changed. */
  let capture: Conversion | null = null;
  /** The recording Save keeps once the page shows what was saved. */
  let result: File | null = null;
  /** The capture's channels, as the converter last described them. */
  let lastDescribed: Described | null = null;

  /** Where a capture a merge takes came from: what shapes it alone again, once it is the only one left. */
  interface FromCapture {
    kind: SourceKind;
    files: File[];
    described: Described;
    converted: Conversion;
  }
  /** A recording a merge takes: a capture's first conversion, or a recording from this computer. */
  interface Input {
    /** As the page names it: an export's folder, or the file's name. Unique among the inputs. */
    name: string;
    /** The file's name without its extension, which prefixes a channel name an earlier input took. */
    stem: string;
    file: File;
    /** Its sidecar's files, where they came with it. */
    sidecar: File[];
    src: string;
    info: RecordingInfo;
    from: FromCapture | null;
  }
  /** The recording the stage shows, where it is one from this computer that another can join. */
  let onStage: { name: string; file: File; sidecar: File[]; from: FromCapture | null } | null = null;
  /** The recordings being merged, two or more; empty otherwise. */
  let inputs: Input[] = [];
  /** Their preview, which the view shapes. */
  let merged: MergedSource | null = null;

  const end = () => {
    worker?.terminate();
    worker = null;
  };
  const idle = () => {
    running.hidden = true;
    settingsForm.inert = false;
  };
  /** Puts away whatever the page was showing or doing, for what was just asked. */
  const reset = () => {
    opened++;
    editor?.close();
    editor = null;
    shapedView = null;
    end();
    idle();
    clear();
    settingsForm.hidden = true;
    stage.hidden = true;
    adding.hidden = true;
    here.replaceChildren();
  };

  /** Opens `src`, with its sidecar at `annotations` or a local recording's own, at annotation `at`
   * if one is given, over `span` ("1000 2000" in ticks, or times) if that is. A converted recording
   * shows with its Save button, which takes the focus; another, its view. */
  async function open(
    src: string,
    annotations: string | null,
    name: string,
    link: boolean,
    converted?: (view: TickwaveView) => void,
    at: string | null = null,
    span: string | null = null,
    hand: File | null = null,
  ): Promise<void> {
    const attachment = link ? githubAttachment(src) : null;
    if (attachment !== null) {
      showAttached(src, attachment);
      return;
    }
    reset();
    // A converted capture keeps its settings in view, to convert again with others.
    settingsForm.hidden = !converted;
    const token = opened;
    let info: RecordingInfo;
    try {
      info = await recordingInfo(src);
    } catch (e) {
      if (token === opened) say(converted ? `The recording was made, but it won't open: ${e instanceof Error ? e.message : e}` : explain(e, src, name));
      return;
    }
    if (token !== opened) return;
    showFacts(facts, info, name);
    const view = document.createElement("tickwave-view");
    view.setAttribute("src", src);
    if (annotations) view.setAttribute("annotations", annotations);
    view.setAttribute("open", "");
    const sidecar: string[] = [];
    view.addEventListener("tickwave-error", (e) => {
      const error = (e as CustomEvent<{ error: unknown; message: string }>).detail.error;
      if (error instanceof Error && error.name === "TickwaveSidecarError") {
        sidecar.push(...error.message.split("\n"));
        say("The sidecar has problems, so the recording shows without its annotations:", sidecar);
      } else say(explain(error, src, name));
    });
    here.append(view);
    const recording = link ? decodeURIComponent(new URL(src, location.href).pathname.split("/").pop() || "recording.tickwave") : name;
    editor = attachEditor(view as TickwaveView, info, { src, recording, annotations, file: hand, save: saveTo });
    const spec = parseWindow(span);
    const ticks = spec?.kind === "span" ? { start: toTicks(spec.start, info.timebase), end: toTicks(spec.end, info.timebase) } : null;
    if (at) void (view as TickwaveView).show(at, ticks);
    else if (span) view.setAttribute("window", span);
    saving.hidden = found.hidden = !converted;
    stage.hidden = false;
    // A recording from this computer can take another to merge with; one by its address can't.
    adding.hidden = link;
    attached.hidden = true;
    // The recording, not the forms that asked for it: they fold away.
    opener.classList.add("folds");
    opener.open = false;
    if (converted) converted(view as TickwaveView);
    else
      // Once the view has drawn it comes into sight with the focus, so its keys work at once.
      view.addEventListener(
        "tickwave-ready",
        () => {
          if (token !== opened) return;
          view.scrollIntoView({ block: "nearest", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
          view.focus({ preventScroll: true });
        },
        { once: true },
      );
    if (link) {
      const q = new URLSearchParams({ src, ...(annotations && { annotations }), ...(at && { annotation: at }), ...(span && { window: span }), ...(saveTo && { save: saveTo }) });
      history.replaceState(null, "", `?${q}${local ? "&local" : ""}`);
      share.textContent = "";
      share.append("A link to this view: ", Object.assign(document.createElement("a"), { href: location.href, textContent: location.href }));
    } else {
      history.replaceState(null, "", location.pathname);
      share.textContent = "Files from this computer can't be shared by link.";
    }
  }

  /** Whether to go on when another recording would put away unsaved annotations. */
  const leaveEdits = () => !editor?.unsaved || confirm("The annotations have changes that aren't saved. Open another recording and lose them?");

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (!leaveEdits()) return;
    const src = srcField.value.trim();
    const annotations = sidecarField.value.trim() || null;
    pick.hidden = true;
    inputs = [];
    merged = null;
    onStage = null;
    if (src) void open(src, annotations, src, true);
  });

  /** A GitHub attachment's two steps, download and choose, since the page can't read it. The
   * address bar then holds the link that shows them. */
  function showAttached(src: string, name: string): void {
    reset();
    pick.hidden = true;
    byId("attached-name").textContent = name;
    attachedLink.href = src;
    attached.hidden = false;
    opener.classList.add("folds");
    opener.open = false;
    history.replaceState(null, "", `?${new URLSearchParams({ src })}`);
    byId("attached-share").replaceChildren("A link that opens these steps: ", Object.assign(document.createElement("a"), { href: location.href, textContent: location.href }));
    attachedLink.focus();
  }

  /** What was chosen or dropped: a recording opens, a capture converts first, and a zip opens as
   * its files would. */
  function take(list: File[], folder: string | null): void {
    if (!leaveEdits()) return;
    pick.hidden = true;
    inputs = [];
    merged = null;
    const zips = list.filter((f) => ZIP.test(f.name));
    if (zips.length > 0) unpack(zips, list.filter((f) => !zips.includes(f)), folder);
    else takeFiles(list, folder);
  }

  /** The zips' files, out of their folders, unzipped one zip at a time, then taken with `rest`, by
   * `then`. A Saleae export's files take the zip's name if no folder gives one. */
  function unpack(zips: File[], rest: File[], folder: string | null, then: (files: File[], folder: string | null) => void = takeFiles): void {
    if (then === takeFiles) reset();
    const [zip, ...more] = zips;
    job({ op: "unzip", file: zip, ends: OPENED }, (m) => {
      if (m.op === "failed") {
        end();
        say(`${zip.name} couldn't be unzipped: ${m.message}.`);
      } else if (m.op === "unzipped") {
        end();
        if (m.files.length === 0) {
          say(`${zip.name} holds nothing to open or convert: no .tickwave recording, and no capture (a Saleae export's .bin files, or a sigrok .sr or .vcd file).`);
          return;
        }
        const files = [...rest, ...m.files];
        const name = folder ?? zip.name.replace(ZIP, "");
        if (more.length > 0) unpack(more, files, name, then);
        else then(files, name);
      }
    });
  }

  function takeFiles(list: File[], folder: string | null): void {
    if (list.some((f) => RECORDING.test(f.name)) || (list.length > 0 && list.every((f) => SIDECAR.test(f.name)))) openFiles(list);
    else if (list.some((f) => CAPTURE.test(f.name))) choose(list, folder);
    else {
      reset();
      say("There's nothing here to open or convert. Open a .tickwave recording, or convert a capture: a Saleae export's folder, or a sigrok .sr or .vcd file.");
    }
  }

  /** A recording and the .toml and .jsonl files of its sidecar; or, when there are several
   * recordings, a list of them to open one at a time. */
  function openFiles(all: File[]): void {
    const sidecar = all.filter((f) => SIDECAR.test(f.name));
    const recordings = all.filter((f) => !sidecar.includes(f));
    if (recordings.length === 0) {
      reset();
      say("Choose a .tickwave recording too: the sidecar's files alone have nothing to draw.");
      return;
    }
    const tomls = sidecar.filter((f) => /\.toml$/i.test(f.name));
    const jsonl = sidecar.filter((f) => !tomls.includes(f));
    /** The recording's own sidecar, named after it; or the only one, beside the only recording. */
    const handOf = (r: File) =>
      tomls.find((t) => t.name.toLowerCase() === r.name.replace(RECORDING, ".annotations.toml").toLowerCase()) ?? (recordings.length === 1 && tomls.length === 1 ? tomls[0] : null);
    const openOne = (r: File) => {
      const hand = handOf(r);
      const sidecar = hand ? [hand, ...jsonl] : [];
      onStage = { name: r.name, file: r, sidecar, from: null };
      void open(registerFile(r, sidecar), null, r.name, false, undefined, null, null, hand);
    };
    if (recordings.length === 1) {
      openOne(recordings[0]);
      return;
    }
    reset();
    recordings.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true }));
    /** The choice shown, which stays chosen if the reader keeps unsaved annotations. */
    let shown: HTMLInputElement | null = null;
    byId("pick-title").textContent = `There are ${recordings.length} recordings here. Choose one to open:`;
    pickList.replaceChildren(
      ...recordings.map((r) => {
        const input = Object.assign(document.createElement("input"), { type: "radio", name: "pick" });
        input.addEventListener("change", () => {
          if (!input.checked) return;
          if (!leaveEdits()) {
            if (shown) shown.checked = true;
            return;
          }
          shown = input;
          openOne(r);
        });
        const label = document.createElement("label");
        label.append(input, ` ${r.name}`);
        if (handOf(r)) label.append(Object.assign(document.createElement("span"), { className: "quiet", textContent: ", with its annotations" }));
        return label;
      }),
    );
    // The list is what the page shows now: what was above it goes, so nothing moves it once a
    // recording opens below.
    attached.hidden = true;
    opener.classList.add("folds");
    opener.open = false;
    pick.hidden = false;
    pickList.querySelector("input")?.focus();
  }

  /** Starts one job in a worker of its own. */
  const job = (message: ToWorker, on: (m: FromWorker) => void) => {
    end();
    const w = new Worker(new URL("./tickwave-convert-worker.js", import.meta.url), { type: "module", name: "tickwave converter" });
    worker = w;
    w.onmessage = (e: MessageEvent<FromWorker>) => {
      if (worker === w) on(e.data);
    };
    w.onerror = (e) => {
      if (worker !== w) return;
      e.preventDefault();
      end();
      idle();
      say(`The converter stopped: ${e.message || "its worker failed to start"}.`);
    };
    w.postMessage(message);
  };

  /** A capture: what it holds, read in a worker, then the settings to convert it with. */
  function choose(list: File[], folder: string | null): void {
    reset();
    capture = null;
    const sr = list.filter((f) => /\.sr$/i.test(f.name));
    const vcd = list.filter((f) => /\.vcd$/i.test(f.name));
    const bin = list.filter((f) => /\.bin$/i.test(f.name));
    const singles = [...sr, ...vcd];
    if (singles.length > 1 || (singles.length === 1 && bin.length > 0)) {
      say("Choose one capture at a time: a Saleae export's folder, or one .sr or .vcd file.");
      return;
    }
    if (singles.length === 1) {
      kind = sr.length ? "sigrok" : "vcd";
      files = singles;
      stem = singles[0].name.replace(/\.(sr|vcd)$/i, "");
    } else {
      kind = "saleae";
      files = bin;
      stem = folder ?? "capture";
    }
    job({ op: "describe", kind, files }, (m) => {
      if (m.op === "described") {
        end();
        // Every channel at once, as the capture names them: the view shows the result, and the
        // table under it shapes the next conversion.
        lastDescribed = m.described;
        if (show(m.described)) run(settings() as Settings);
      } else if (m.op === "failed") {
        end();
        say(`${kind === "saleae" ? "The export" : files[0].name} couldn't be read: ${m.message}`);
      }
    });
  }

  /** The capture's channels in the table. False when it has none to take. */
  function show(described: Described): boolean {
    channels = described.channels;
    const what = kind === "saleae" ? "a Saleae Logic 2 binary export" : kind === "sigrok" ? "a sigrok session" : "a value change dump";
    byId("source").textContent = kind === "saleae" ? stem : files[0].name;
    byId("source-facts").textContent = `${what}, with ${channels.length === 1 ? "one channel" : `${channels.length} channels`} tickwave can take.`;
    const skipped = described.skipped.map((s) => `${s.name}: ${s.reason}`);
    if (channels.length === 0) {
      say(`${byId("source").textContent} has no channels tickwave can take.`, skipped);
      return false;
    }
    rows.replaceChildren(
      ...channels.map((c) =>
        channelRow(c.name, c.name, c.analog, c.unit, c.analog ? `analog${c.count === null ? "" : `, ${count(c.count)} samples`}` : `digital${c.count === null ? "" : `, ${count(c.count)} edges`}`),
      ),
    );
    skippedBox.textContent = skipped.length ? `Left out: ${skipped.join("; ")}.` : "";
    skippedBox.hidden = skipped.length === 0;
    rateRow.hidden = !described.rateMissing;
    rateField.value = "";
    return true;
  }

  const field = (cls: string, label: string, value = "", size?: number) => {
    const f = Object.assign(document.createElement("input"), { type: "text", value, spellcheck: false, className: cls });
    if (size) f.size = size;
    f.setAttribute("aria-label", label);
    return f;
  };
  const cell = (...els: (Node | string)[]) => {
    const td = document.createElement("td");
    td.append(...els);
    return td;
  };

  /** A channel's row, `key` the view's name for it and `own` its source's: whether to take it, its
   * name and unit, and its move and kept span. */
  function channelRow(key: string, own: string, analog: boolean, unit: string | null, about: string): HTMLTableRowElement {
    const tr = document.createElement("tr");
    tr.dataset.source = key;
    const take = Object.assign(document.createElement("input"), { type: "checkbox", checked: true, className: "take" });
    take.setAttribute("aria-label", `Take ${key}`);
    take.addEventListener("change", showTaken);
    const where = cell(own, " ", Object.assign(document.createElement("span"), { className: "quiet", textContent: about }));
    const shape = [field("offset", `Move ${key} by`, "", 9), field("from", `Keep ${key} from`, "", 11), field("to", `Keep ${key} to`, "", 11)];
    for (const f of shape) f.addEventListener("change", () => shapeRow(tr));
    tr.append(cell(take), where, cell(field("name", `Name for ${key}`, key)), cell(...(analog ? [field("unit", `Unit for ${key}`, unit ?? "", 4)] : [])), ...shape.map((f) => cell(f)));
    return tr;
  }

  const input = (tr: Element, cls: string) => tr.querySelector<HTMLInputElement>(`input.${cls}`);
  /** The table's channel rows, without a merge's head rows. */
  const rowsOf = () => [...rows.querySelectorAll<HTMLTableRowElement>("tr[data-source]")];

  /** A time from the table in ticks: ticks as written, or a time such as -40 ns. Null when empty,
   * NaN when it reads as neither. */
  function ticksOf(text: string, tb: { num: number; den: number }): number | null {
    const t = text.trim();
    if (t === "") return null;
    if (/^-?\d+$/.test(t)) return Number(t);
    const p = parseTime(t);
    return p ? toTicks(p, tb) : NaN;
  }
  const timeOf = (ticks: number, tb: { num: number; den: number }) => `${ticks < 0 ? "-" : ""}${timeText(Math.abs(ticks), tb)}`;

  /** The view shows the taken channels, in the table's order. */
  function showTaken(): void {
    const v = shapedView;
    if (!v) return;
    const names = rowsOf()
      .filter((tr) => input(tr, "take")!.checked)
      .map((tr) => shownNames.get(tr.dataset.source!))
      .filter((n): n is string => n !== undefined);
    // A derived channel shows while both its operands are taken, after its first.
    for (const c of merged?.mergedInfo.channels ?? []) {
      if (c.kind !== "analog" || !c.product || !c.product.every((n) => names.includes(n))) continue;
      names.splice(names.indexOf(c.product[0]) + 1, 0, c.name);
    }
    if (names.length > 0) v.setAttribute("channels", names.join(" "));
  }

  /** A row's move and trim, from its fields, on the view. */
  function shapeRow(tr: HTMLTableRowElement): void {
    const v = shapedView;
    // A shape goes by the capture's name for the channel, as the row does.
    const source = tr.dataset.source!;
    const tb = v ? recordingTimebase : null;
    if (!v || !shownNames.has(source) || !tb) return;
    const [offset, from, to] = ["offset", "from", "to"].map((cls) => ticksOf(input(tr, cls)!.value, tb));
    const bad = [offset, from, to].some((t) => Number.isNaN(t));
    for (const cls of ["offset", "from", "to"]) input(tr, cls)!.toggleAttribute("aria-invalid", bad && Number.isNaN(ticksOf(input(tr, cls)!.value, tb)));
    if (bad) {
      say(`${tr.dataset.source}: a time is ticks, or a decimal with a unit, such as -40 ns or 1.2 ms.`);
      return;
    }
    clear();
    v.shape = { [source]: { offset: offset ?? 0, keep: from === null && to === null ? null : { from: from ?? -Infinity, to: to ?? Infinity } } };
    fillShape(v.shape);
  }

  /** The table's fields from the view's shapes, and its rows in the view's order. */
  function fillShape(shape: Record<string, ChannelShape>, order?: readonly string[]): void {
    const tb = recordingTimebase;
    if (!tb) return;
    const source = new Map([...shownNames].map(([s, n]) => [n, s]));
    for (const tr of rowsOf()) {
      const s = shape[tr.dataset.source!];
      if (!s) continue;
      input(tr, "offset")!.value = s.offset === 0 ? "" : timeOf(s.offset, tb);
      input(tr, "from")!.value = s.keep ? timeOf(s.keep.from, tb) : "";
      input(tr, "to")!.value = s.keep ? timeOf(s.keep.to, tb) : "";
    }
    if (order) {
      const byName = new Map(rowsOf().map((tr) => [tr.dataset.source!, tr]));
      const ordered = order.map((n) => byName.get(source.get(n) ?? "")).filter((tr): tr is HTMLTableRowElement => tr !== undefined);
      const all = [...ordered, ...rowsOf().filter((tr) => !ordered.includes(tr))];
      // A merge's rows stay under their recording's head.
      const heads = [...rows.querySelectorAll<HTMLTableRowElement>("tr.input")];
      if (heads.length === 0) rows.append(...all);
      else for (const h of heads) rows.append(h, ...all.filter((tr) => tr.dataset.input === h.dataset.input));
    }
  }

  /** The metadata field's lines, or what is wrong with one. */
  function metaOf(): [string, string][] | string {
    const meta: [string, string][] = [];
    for (const line of byId<HTMLTextAreaElement>("meta").value.split("\n")) {
      if (!line.trim()) continue;
      const at = line.indexOf("=");
      const [k, v] = at < 0 ? ["", ""] : [line.slice(0, at).trim(), line.slice(at + 1).trim()];
      if (!k || !v) return `Each metadata line is key = value; this one isn't: ${line.trim()}`;
      meta.push([k, v]);
    }
    return meta;
  }

  /** The form as the converter's settings, or what is wrong with it. */
  function settings(): Settings | string {
    const taken: [string, string][] = [];
    const units: [string, string][] = [];
    // In the table's order, which follows the rows' order in the view.
    for (const tr of rowsOf()) {
      if (!input(tr, "take")!.checked) continue;
      const c = channels.find((c) => c.name === tr.dataset.source)!;
      const to = input(tr, "name")!.value.trim();
      taken.push([c.name, to]);
      const unit = input(tr, "unit");
      const u = unit?.value.trim() ?? "";
      if (unit && u && u !== (c.unit ?? "")) units.push([to, u]);
    }
    if (taken.length === 0) return "Take at least one channel.";
    const blank = taken.find(([, to]) => to === "");
    if (blank) return `Give ${blank[0]} a name.`;
    const names = taken.map(([, to]) => to);
    const twice = names.find((n, i) => names.indexOf(n) !== i);
    if (twice) return `Two channels are named ${twice}: each needs its own name.`;
    const meta = metaOf();
    if (typeof meta === "string") return meta;
    return {
      ...DEFAULTS,
      channels: taken,
      title: byId<HTMLInputElement>("title").value.trim() || null,
      meta,
      level: byId<HTMLInputElement>("level").valueAsNumber,
      blockKib: byId<HTMLInputElement>("block").valueAsNumber,
      units,
      codes: byId<HTMLInputElement>("codes").checked,
      rate: rateRow.hidden ? null : rateField.value.trim() || null,
    };
  }

  const label = (pass: Pass, channel: string, merging: boolean) =>
    ({
      check: "Checking the export's edges",
      codes: `Looking for exact codes in ${channel}`,
      encode: merging ? "Merging" : "Encoding",
      verify: merging ? "Checking the merged recording against its recordings" : "Checking the recording against the capture",
    })[pass];

  function run(chosen: Settings): void {
    work({ op: "convert", kind, files, settings: chosen }, (m) => {
      if (m.op === "converted") converted(new File([m.file], `${stem}.tickwave`), m.codes, m.seconds, chosen);
    });
  }

  /** Runs a conversion or a merge with its progress onStage, and hands `done` what it made. */
  function work(message: ToWorker, done: (m: Extract<FromWorker, { op: "converted" | "merged" }>) => void): void {
    clear();
    settingsForm.inert = true;
    running.hidden = false;
    byId("pass").textContent = "Starting";
    byId("count").textContent = "";
    bar.removeAttribute("value");
    let total = 0;
    let taken = 0;
    job(message, (m) => {
      switch (m.op) {
        case "start":
          byId("pass").textContent = label(m.pass, m.channel, message.op === "merge");
          total = m.total;
          taken = 0;
          bar.max = Math.max(total, 1);
          bar.value = 0;
          byId("count").textContent = "";
          break;
        case "step":
          taken += m.n;
          bar.value = taken;
          byId("count").textContent = `${count(taken)} of ${count(total)}`;
          break;
        case "converted":
        case "merged":
          end();
          idle();
          done(m);
          break;
        case "failed":
          end();
          idle();
          say(`${message.op === "merge" ? "The merge" : "The conversion"} failed: ${m.message}`);
          break;
      }
    });
  }

  /** A conversion done: the capture's first is shaped next; a later one, made by Save, is saved. */
  function converted(file: File, codes: [string, number | null][], seconds: number, chosen: Settings): void {
    if (!capture) {
      capture = { file, codes, seconds, settings: chosen };
      for (const tr of rowsOf()) for (const cls of ["offset", "from", "to"]) input(tr, cls)!.value = "";
      shapeCapture();
    } else {
      download(file);
      showSaved(file, codes, seconds);
    }
  }

  /** What the conversion found, beside Save. */
  function conversionFacts(file: File, codes: [string, number | null][], seconds: number, report: string[] | null = null): void {
    const exact = codes.filter(([, bits]) => bits !== null).map(([n, bits]) => `${n} (${bits}-bit)`);
    const f32 = codes.filter(([, bits]) => bits === null).map(([n]) => n);
    byId("took").textContent = `${formatBytes(file.size)}, ${report ? "merged" : "converted"} and checked in ${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} s`;
    found.textContent = report ? report.join(" ") : [exact.length ? `Exact codes: ${exact.join(", ")}.` : "", f32.length ? `Stored as f32: ${f32.join(", ")}.` : ""].filter(Boolean).join(" ");
  }

  /** The capture in the view, shaped by the table: moved, trimmed and reordered as it says. */
  function shapeCapture(): void {
    const c = capture!;
    result = null;
    merging(false);
    onStage = { name: kind === "saleae" ? stem : files[0].name, file: c.file, sidecar: [], from: { kind, files, described: lastDescribed!, converted: c } };
    void open(registerFile(c.file, []), null, c.file.name, false, (view) => {
      shownNames = new Map(c.settings.channels);
      shapedView = view;
      view.shaping = true;
      void recordingInfo(view.getAttribute("src")!).then((info) => {
        recordingTimebase = info.timebase;
        // The table's moves and trims, and its order, from a shaping before.
        for (const tr of rowsOf()) shapeRow(tr);
        showTaken();
      });
      view.addEventListener("tickwave-shape", (e) => {
        const d = (e as CustomEvent<{ shape: Record<string, ChannelShape>; channels: string[] }>).detail;
        fillShape(d.shape, d.channels);
      });
      // Annotations belong to the recording saved, not to the capture being shaped.
      byId("edit").parentElement!.hidden = true;
      reshape.hidden = true;
      save.textContent = `Save ${c.file.name}`;
      conversionFacts(c.file, c.codes, c.seconds);
      // The next thing to do is save it: the focus goes there, and brings it into sight.
      save.focus();
    });
  }

  /** The recording saved, as a recording opened: annotations can be edited on it. A merge's
   * `report` says what it left out or renamed. */
  function showSaved(file: File, codes: [string, number | null][], seconds: number, report: string[] | null = null): void {
    result = file;
    onStage = { name: file.name, file, sidecar: [], from: null };
    void open(registerFile(file, []), null, file.name, false, () => {
      settingsForm.hidden = true;
      reshape.hidden = false;
      save.textContent = `Save ${file.name}`;
      conversionFacts(file, codes, seconds, report);
      reshape.focus();
    });
  }

  /** Offers `file` to save. */
  function download(file: File): void {
    const a = Object.assign(document.createElement("a"), { href: URL.createObjectURL(file), download: file.name });
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 0);
  }

  /** The table's settings with the view's moves and trims, as the converter's times, by name. */
  function shapedSettings(): Settings | string {
    const chosen = settings();
    if (typeof chosen === "string") return chosen;
    const tb = recordingTimebase;
    const shape = shapedView?.shape ?? {};
    const named = new Map(chosen.channels);
    const offsets: [string, string][] = [];
    const keeps: [string, string | null, string | null][] = [];
    for (const [source, s] of Object.entries(shape)) {
      const name = named.get(source);
      if (!name || !tb) continue;
      if (s.offset !== 0) offsets.push([name, timeOf(s.offset, tb)]);
      if (s.keep) keeps.push([name, timeOf(s.keep.from, tb), timeOf(s.keep.to, tb)]);
    }
    return { ...chosen, offsets, keeps };
  }


  // Merging -------------------------------------------------------------------------------------

  /** The table and the form for one capture, or for a merge. */
  function merging(on: boolean): void {
    byId("merge-note").hidden = !on;
    byId("shape-note").hidden = on;
    byId("codes-row").hidden = on;
    byId("from-col").textContent = on ? "In its recording" : "In the capture";
    if (on) {
      rateRow.hidden = true;
      skippedBox.hidden = true;
    }
  }

  /** What the table's rows say, by the channel each shapes, and each recording's alignment. Times
   * go as times with a unit: a tick count means another time on another time axis. */
  function tableFields(): { rows: Map<string, Record<string, string | boolean>>; aligns: Map<string, string> } {
    const tb = recordingTimebase;
    const time = (el: HTMLInputElement) => {
      const t = tb ? ticksOf(el.value, tb) : null;
      return t === null || Number.isNaN(t) || !tb ? el.value : timeOf(t, tb);
    };
    const saved = new Map<string, Record<string, string | boolean>>();
    for (const tr of rowsOf()) {
      const f: Record<string, string | boolean> = { take: input(tr, "take")!.checked };
      for (const cls of ["name", "unit", "offset", "from", "to"]) {
        const el = input(tr, cls);
        if (el) f[cls] = cls === "name" || cls === "unit" ? el.value : time(el);
      }
      saved.set(tr.dataset.source!, f);
    }
    const aligns = new Map([...rows.querySelectorAll<HTMLTableRowElement>("tr.input")].map((h) => [h.dataset.name!, time(input(h, "align")!)]));
    return { rows: saved, aligns };
  }

  function restoreFields(saved: ReturnType<typeof tableFields>): void {
    for (const tr of rowsOf()) {
      const f = saved.rows.get(tr.dataset.source!);
      if (!f) continue;
      input(tr, "take")!.checked = f.take as boolean;
      for (const cls of ["name", "unit", "offset", "from", "to"]) {
        const el = input(tr, cls);
        if (el && typeof f[cls] === "string") el.value = f[cls];
      }
    }
    for (const h of rows.querySelectorAll<HTMLTableRowElement>("tr.input")) input(h, "align")!.value = saved.aligns.get(h.dataset.name!) ?? "";
  }

  /** `file` as a recording a merge takes, named `name`, or `name (2)` where another has that name. */
  async function inputOf(name: string, file: File, sidecar: File[], from: FromCapture | null): Promise<Input> {
    const src = registerFile(file, sidecar);
    const info = await recordingInfo(src);
    let unique = name;
    for (let n = 2; inputs.some((i) => i.name === unique); n++) unique = `${name} (${n})`;
    return { name: unique, stem: file.name.replace(RECORDING, ""), file, sidecar, src, info, from };
  }

  /** Files added to merge with what the stage shows: a recording joins as it is, with its
   * sidecar's files, and a capture converts whole first. */
  async function add(list: File[], folder: string | null): Promise<void> {
    if (!leaveEdits()) return;
    clear();
    const zips = list.filter((f) => ZIP.test(f.name));
    if (zips.length > 0) {
      unpack(zips, list.filter((f) => !zips.includes(f)), folder, (files, name) => void add(files, name));
      return;
    }
    const saved = tableFields();
    const before = inputs;
    const giveUp = (lead: string) => {
      inputs = before;
      say(lead);
    };
    try {
      if (inputs.length === 0) {
        if (!onStage) return;
        inputs = [await inputOf(onStage.name, onStage.file, onStage.sidecar, onStage.from)];
      }
      const sidecar = list.filter((f) => SIDECAR.test(f.name));
      const tomls = sidecar.filter((f) => /\.toml$/i.test(f.name));
      const jsonl = sidecar.filter((f) => !tomls.includes(f));
      const recordings = list.filter((f) => RECORDING.test(f.name));
      for (const r of recordings) {
        const hand = tomls.find((t) => t.name.toLowerCase() === r.name.replace(RECORDING, ".annotations.toml").toLowerCase()) ?? (recordings.length === 1 && tomls.length === 1 ? tomls[0] : null);
        inputs = [...inputs, await inputOf(r.name, r, hand ? [hand, ...jsonl] : [], null)];
      }
      const captures = list.filter((f) => CAPTURE.test(f.name));
      if (captures.length > 0) convertToAdd(captures, folder, saved, before);
      else if (recordings.length > 0) await showMerge(saved, before);
      else giveUp("There's nothing here to add. Add a .tickwave recording, or a capture: a Saleae export's folder, or a sigrok .sr or .vcd file.");
    } catch (e) {
      giveUp(`That can't be added: ${e instanceof Error ? e.message : e}`);
    }
  }

  /** A capture to add, converted whole, every channel under its own name, as the first capture is. */
  function convertToAdd(captures: File[], folder: string | null, saved: ReturnType<typeof tableFields>, before: Input[]): void {
    const sr = captures.filter((f) => /\.sr$/i.test(f.name));
    const vcd = captures.filter((f) => /\.vcd$/i.test(f.name));
    const bin = captures.filter((f) => /\.bin$/i.test(f.name));
    const singles = [...sr, ...vcd];
    const giveUp = (lead: string) => {
      end();
      inputs = before;
      say(lead);
    };
    if (singles.length > 1 || (singles.length === 1 && bin.length > 0)) {
      giveUp("Add one capture at a time: a Saleae export's folder, or one .sr or .vcd file.");
      return;
    }
    const k: SourceKind = singles.length === 0 ? "saleae" : sr.length > 0 ? "sigrok" : "vcd";
    const fs = singles.length === 0 ? bin : singles;
    const name = singles.length === 0 ? (folder ?? "capture") : singles[0].name;
    const own = singles.length === 0 ? name : name.replace(/\.(sr|vcd)$/i, "");
    job({ op: "describe", kind: k, files: fs }, (m) => {
      if (m.op === "failed") giveUp(`${name} couldn't be read: ${m.message}`);
      if (m.op !== "described") return;
      end();
      const described = m.described;
      if (described.channels.length === 0) {
        giveUp(`${name} has no channels tickwave can take.`);
        return;
      }
      const chosen: Settings = { ...DEFAULTS, channels: described.channels.map((c) => [c.name, c.name]) };
      work({ op: "convert", kind: k, files: fs, settings: chosen }, (done) => {
        if (done.op !== "converted") return;
        const file = new File([done.file], `${own}.tickwave`);
        const converted = { file, codes: done.codes, seconds: done.seconds, settings: chosen };
        inputOf(name, file, [], { kind: k, files: fs, described, converted }).then(
          (i) => {
            inputs = [...inputs, i];
            return showMerge(saved, before);
          },
          (e) => giveUp(`${name} was converted, but it won't open: ${e instanceof Error ? e.message : e}`),
        );
      });
    });
  }

  /** The merged recording's name, without .tickwave. */
  const mergedName = () => inputs.map((i) => i.stem).join("+");

  /** The recordings being merged, on one time axis in the view, which shapes them, and the table
   * that follows it. Where they can't share an axis, the page goes back to `before`. */
  async function showMerge(saved = tableFields(), before: Input[] = inputs): Promise<void> {
    let preview: { src: string; merged: MergedSource };
    try {
      preview = await previewMerge(inputs.map((i) => ({ src: i.src, stem: i.stem })));
    } catch (e) {
      inputs = before;
      say(`These recordings can't be merged: ${e instanceof Error ? e.message : e}`);
      return;
    }
    const m = preview.merged;
    const info = m.mergedInfo;
    merged = m;
    result = null;
    onStage = null;
    mergeTable(info, m);
    restoreFields(saved);
    void open(preview.src, null, inputs.map((i) => i.name).join(" + "), false, (view) => {
      shapedView = view;
      shownNames = new Map(info.channels.map((c) => [c.name, c.name]));
      recordingTimebase = info.timebase;
      view.groups = inputs.map((input, k) => ({ name: input.name, channels: info.channels.filter((_, i) => m.origin(i).input === k).map((c) => c.name) }));
      view.shaping = true;
      for (const tr of rowsOf()) shapeRow(tr);
      for (const h of rows.querySelectorAll<HTMLTableRowElement>("tr.input")) alignRow(h);
      showTaken();
      view.addEventListener("tickwave-shape", (e) => {
        const d = (e as CustomEvent<{ shape: Record<string, ChannelShape>; aligns: Record<string, number>; channels: string[] }>).detail;
        fillShape(d.shape, d.channels);
        fillAligns(d.aligns);
      });
      byId("edit").parentElement!.hidden = true;
      reshape.hidden = true;
      save.textContent = `Save ${mergedName()}.tickwave`;
      byId("took").textContent = "";
      found.textContent = "";
      save.focus();
    });
  }

  /** The table for a merge: each recording's head, then its channels' rows. A derived channel
   * comes with its operands, and has no row. */
  function mergeTable(info: RecordingInfo, m: MergedSource): void {
    merging(true);
    byId("source").textContent = `Merging ${inputs.length} recordings`;
    const ticks = inputs.map((i) => `${i.name}, ${formatInterval(readingStep(i.info), i.info.timebase, readingStep(i.info))}`);
    byId("source-facts").textContent = `On one time axis, each at its own tick: ${ticks.join("; ")}.`;
    rows.replaceChildren(
      ...inputs.flatMap((recording, k) => {
        const head = document.createElement("tr");
        head.className = "input";
        head.dataset.input = String(k);
        head.dataset.name = recording.name;
        const th = Object.assign(document.createElement("th"), { colSpan: 7, scope: "colgroup" });
        const align = field("align", `Align ${recording.name} by`, "", 11);
        align.addEventListener("change", () => alignRow(head));
        const label = document.createElement("label");
        label.append("Aligned by ", align);
        const remove = Object.assign(document.createElement("button"), { type: "button", className: "button", textContent: "Remove" });
        remove.setAttribute("aria-label", `Remove ${recording.name}`);
        remove.addEventListener("click", () => removeInput(k));
        th.append(recording.name, label, remove);
        head.append(th);
        const own = info.channels.flatMap((c, i) => {
          const o = m.origin(i);
          return o.input === k && !(c.kind === "analog" && c.product) ? [{ c, mine: recording.info.channels[o.channel] }] : [];
        });
        return [
          head,
          ...own.map(({ c, mine }) => {
            const tr = channelRow(c.name, mine.name, c.kind === "analog", c.kind === "analog" ? c.unit : null, c.kind === "analog" ? `analog${c.unit ? `, ${c.unit}` : ""}` : "digital");
            tr.dataset.input = String(k);
            tr.dataset.own = mine.name;
            return tr;
          }),
        ];
      }),
    );
  }

  /** A recording's alignment, from its head's field, on the view. */
  function alignRow(head: HTMLTableRowElement): void {
    const v = shapedView;
    const tb = recordingTimebase;
    const f = input(head, "align")!;
    if (!v || !tb) return;
    const ticks = ticksOf(f.value, tb);
    f.toggleAttribute("aria-invalid", Number.isNaN(ticks));
    if (Number.isNaN(ticks)) {
      say(`${head.dataset.name}: a time is ticks, or a decimal with a unit, such as -40 ns or 1.2 ms.`);
      return;
    }
    clear();
    v.aligns = { [head.dataset.name!]: ticks ?? 0 };
  }

  /** The heads' fields from the view's alignments. */
  function fillAligns(aligns: Record<string, number>): void {
    const tb = recordingTimebase;
    if (!tb) return;
    for (const h of rows.querySelectorAll<HTMLTableRowElement>("tr.input")) {
      const a = aligns[h.dataset.name!] ?? 0;
      input(h, "align")!.value = a === 0 ? "" : timeOf(a, tb);
    }
  }

  /** Takes recording `k` out of the merge. With one left, the page shapes or shows it alone. */
  function removeInput(k: number): void {
    const saved = tableFields();
    inputs = inputs.filter((_, i) => i !== k);
    if (inputs.length >= 2) {
      void showMerge(saved);
      return;
    }
    const [last] = inputs;
    inputs = [];
    merged = null;
    if (last.from) {
      ({ kind, files, described: lastDescribed, converted: capture } = last.from);
      stem = last.stem;
      show(last.from.described);
      restoreFields(saved);
      shapeCapture();
    } else {
      rows.replaceChildren();
      merging(false);
      onStage = { name: last.name, file: last.file, sidecar: last.sidecar, from: null };
      void open(last.src, null, last.name, false, undefined, null, null, last.sidecar.find((f) => /\.toml$/i.test(f.name)) ?? null);
    }
  }

  /** The table and the view as the merge's settings, or what is wrong with them. */
  function mergeSettings(): { inputs: MergeInput[]; settings: MergeSettings } | string {
    const v = shapedView;
    const tb = recordingTimebase;
    const m = merged;
    if (!v || !tb || !m) return "The merge isn't ready yet.";
    const shape = v.shape;
    const aligns = v.aligns;
    const info = m.mergedInfo;
    const out: MergeInput[] = inputs.map((i) => ({ file: i.file, stem: i.stem, align: aligns[i.name] ? timeOf(aligns[i.name], tb) : null, channels: [], sidecar: i.sidecar }));
    const names: string[] = [];
    const units: [string, string][] = [];
    const offsets: [string, string][] = [];
    const keeps: [string, string | null, string | null][] = [];
    for (const tr of rowsOf()) {
      if (!input(tr, "take")!.checked) continue;
      const k = Number(tr.dataset.input);
      const key = tr.dataset.source!;
      const to = input(tr, "name")!.value.trim();
      if (!to) return `Give ${key} a name.`;
      out[k].channels.push([tr.dataset.own!, to]);
      names.push(to);
      const c = info.channels.find((c) => c.name === key);
      const u = input(tr, "unit")?.value.trim() ?? "";
      if (c?.kind === "analog" && u && u !== c.unit) units.push([to, u]);
      const s = shape[key];
      if (s && s.offset !== 0) offsets.push([to, timeOf(s.offset, tb)]);
      // A kept span in the view's ticks is a whole number of the recording's own.
      if (s?.keep) {
        const f = m.factors[k];
        const own = inputs[k].info.timebase;
        keeps.push([to, timeOf(Math.round(s.keep.from / f), own), timeOf(Math.round(s.keep.to / f), own)]);
      }
    }
    const empty = out.findIndex((i) => i.channels.length === 0);
    if (empty >= 0) return `Take at least one channel of ${inputs[empty].name}, or remove it.`;
    const twice = names.find((n, i) => names.indexOf(n) !== i);
    if (twice) return `Two channels are named ${twice}: each needs its own name.`;
    const meta = metaOf();
    if (typeof meta === "string") return meta;
    const title = byId<HTMLInputElement>("title").value.trim() || null;
    return { inputs: out, settings: { stem: mergedName(), title, meta, level: byId<HTMLInputElement>("level").valueAsNumber, blockKib: byId<HTMLInputElement>("block").valueAsNumber, units, offsets, keeps } };
  }

  function runMerge(chosen: { inputs: MergeInput[]; settings: MergeSettings }): void {
    work({ op: "merge", ...chosen }, (done) => {
      if (done.op !== "merged") return;
      const file = new File([done.file], `${mergedName()}.tickwave`);
      download(file);
      showSaved(file, [], done.seconds, done.report);
    });
  }

  /** Save: what the page shows when it is the recording to keep, else a conversion or a merge with
   * the table's settings first. */
  save.addEventListener("click", () => {
    if (result) {
      download(result);
      return;
    }
    if (inputs.length >= 2) {
      const chosen = mergeSettings();
      if (typeof chosen === "string") say(chosen);
      else runMerge(chosen);
      return;
    }
    const c = capture;
    if (!c) return;
    const chosen = shapedSettings();
    if (typeof chosen === "string") {
      say(chosen);
      return;
    }
    if (JSON.stringify(chosen) === JSON.stringify({ ...c.settings, offsets: [], keeps: [] })) {
      download(c.file);
      showSaved(c.file, c.codes, c.seconds);
    } else run(chosen);
  });
  reshape.addEventListener("click", () => (inputs.length >= 2 ? void showMerge() : shapeCapture()));

  settingsForm.addEventListener("submit", (e) => {
    e.preventDefault();
    save.click();
  });
  byId("trim-all").addEventListener("click", () => {
    const v = shapedView;
    const sel = v?.selection;
    if (!v || !sel) {
      say("Select a span first: drag along the view's time axis.");
      return;
    }
    clear();
    const next: Record<string, ChannelShape> = {};
    for (const [name, s] of Object.entries(v.shape)) next[name] = { offset: s.offset, keep: { from: sel.start - s.offset, to: sel.end - s.offset } };
    v.shape = next;
    fillShape(v.shape);
  });
  byId("cancel").addEventListener("click", () => {
    end();
    idle();
  });
  filesInput.addEventListener("change", () => {
    take([...(filesInput.files ?? [])], null);
    filesInput.value = "";
  });
  folderInput.addEventListener("change", () => {
    const list = [...(folderInput.files ?? [])];
    // Only the folder's own files: an export keeps none in folders of its own.
    const own = list.filter((f) => f.webkitRelativePath.split("/").length === 2);
    take(own, own[0]?.webkitRelativePath.split("/")[0] ?? null);
    folderInput.value = "";
  });
  addFiles.addEventListener("change", () => {
    void add([...(addFiles.files ?? [])], null);
    addFiles.value = "";
  });
  addFolder.addEventListener("change", () => {
    const list = [...(addFolder.files ?? [])];
    const own = list.filter((f) => f.webkitRelativePath.split("/").length === 2);
    void add(own, own[0]?.webkitRelativePath.split("/")[0] ?? null);
    addFolder.value = "";
  });
  // A capture or recording dropped on the stage joins what it shows.
  stage.addEventListener("dragover", (e) => {
    if (adding.hidden) return;
    e.preventDefault();
    stage.classList.add("over");
  });
  stage.addEventListener("dragleave", () => stage.classList.remove("over"));
  stage.addEventListener("drop", (e) => {
    stage.classList.remove("over");
    if (adding.hidden) return;
    e.preventDefault();
    if (e.dataTransfer) void dropped(e.dataTransfer).then(({ files, folder }) => add(files, folder));
  });
  attachedFile.addEventListener("change", () => {
    take([...(attachedFile.files ?? [])], null);
    attachedFile.value = "";
  });
  for (const zone of [drop, attached]) {
    zone.addEventListener("dragover", (e) => {
      e.preventDefault();
      zone.classList.add("over");
    });
    zone.addEventListener("dragleave", () => zone.classList.remove("over"));
    zone.addEventListener("drop", (e) => {
      e.preventDefault();
      zone.classList.remove("over");
      if (e.dataTransfer) void dropped(e.dataTransfer).then(({ files, folder }) => take(files, folder));
    });
  }

  const q = new URLSearchParams(location.search);
  const src = q.get("src");
  if (src) {
    srcField.value = src;
    sidecarField.value = q.get("annotations") ?? "";
    // A local recording goes by its file's name, not by the address it is served at.
    const name = local ? decodeURIComponent(new URL(src, location.href).pathname.split("/").pop() ?? src) : src;
    void open(src, q.get("annotations"), name, true, undefined, q.get("annotation"), q.get("window"));
  }
}

/** A failure, in plain words. */
function explain(e: unknown, src: string, name: string): string {
  const error = e instanceof Error ? e : new Error(String(e));
  const url = new URL(src, location.href);
  switch (error.name) {
    case "TickwaveNotRecording":
      return `${name} isn't a tickwave recording. The viewer reads .tickwave files, and converts a capture into one first.`;
    case "TickwaveNoLoader":
      return `${name} doesn't end in .tickwave. The viewer reads .tickwave files: give the file's own address, or download it and open it from here.`;
    case "TickwaveNetworkError":
      return url.origin !== location.origin
        ? `${url.host} didn't send the recording. Its server may not let other sites read its files: it has to answer with an Access-Control-Allow-Origin header (CORS). Or it may be out of reach.`
        : `${name} didn't arrive: the server gave no answer.`;
    case "TickwaveHttpError": {
      const status = /HTTP (\d+)/.exec(error.message)?.[1];
      return status === "404" ? `There's no file at ${url.href}: the server answered 404.` : `The server answered ${status ?? "with an error"} for ${url.href}.`;
    }
    default:
      return error.message;
  }
}

// Last, once every binding above has its value.
if (document.body.classList.contains("viewer")) openPage();
