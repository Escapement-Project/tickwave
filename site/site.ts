// tickwave's site: the Open page, where a recording opens, or a capture converts and opens. Built by
// web/scripts/site.ts; "tickwave" is the tickwave.js beside this script. The project's face.js and
// header.js run the header.

import { recordingInfo, registerFile, type Channel, type ChannelShape, type RecordingInfo, type TickwaveView } from "tickwave";
import { parseWindow } from "../web/src/attributes.ts";
import { formatBytes, formatInterval } from "../web/src/readouts.ts";
import { parseTime, toTicks } from "../web/src/time.ts";
import { attachEditor, type Editor } from "./editor.ts";
import { timeText } from "./editor-fields.ts";
import { DEFAULTS, type ChannelInfo, type Described, type FromWorker, type Pass, type Settings, type SourceKind, type ToWorker } from "../web/src/convert/protocol.ts";

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
function channelFacts(c: Channel, tb: { num: number; den: number }): string {
  const notes = c.kind === "analog" ? ["analog", c.unit] : [];
  if (c.offset) {
    const why = c.offset.kind === "delay" ? "for a known delay" : `(${c.offset.kind})`;
    notes.push(`moved ${formatInterval(Math.abs(c.offset.ticks), tb)} ${c.offset.ticks > 0 ? "later" : "earlier"} ${why}`);
  }
  // A recording with a channel moved or cut states each channel's span of source data.
  if (c.kept) notes.push(`source data from ${formatInterval(c.kept.from, tb)} to ${formatInterval(c.kept.to, tb)}`);
  return notes.length > 0 ? `${c.name} (${notes.join(", ")})` : c.name;
}

/** A recording's facts as a definition list. */
function showFacts(facts: HTMLElement, info: RecordingInfo, name: string): void {
  const rows: [string, string][] = [
    ["Recording", info.title || name],
    ["Length", formatInterval(info.length, info.timebase)],
    ["One tick", formatInterval(1, info.timebase)],
    ["Channels", info.channels.map((c) => channelFacts(c, info.timebase)).join(", ")],
  ];
  if (info.bytes !== undefined) rows.push(["Size", formatBytes(info.bytes)]);
  const source = info.source;
  if (source) rows.push(["From", [source.format, ...source.files].join(", ")]);
  if (source?.rate === "inferred") rows.push(["Sample rate", "inferred from the edges"]);
  else if (source?.rate === "given") rows.push(["Sample rate", "given when it was converted"]);
  if (source?.converter) rows.push(["Converted by", source.converter]);
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
  /** Each source channel's name in the recording shown, as the last conversion named it. */
  let shownNames = new Map<string, string>();
  let recordingTimebase: { num: number; den: number } | null = null;
  /** The view of the recording converted here, which the table's moves and trims shape. */
  let shapedView: TickwaveView | null = null;
  let worker: Worker | null = null;
  /** The capture converted whole, every channel under its own name: what the view shapes, and
   * what Save keeps when nothing is changed. */
  let capture: { file: File; codes: [string, number | null][]; seconds: number; settings: Settings } | null = null;
  /** The recording Save keeps once the page shows what was saved. */
  let result: File | null = null;

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
    if (src) void open(src, annotations, src, true);
  });

  /** What was chosen or dropped: a recording opens, a capture converts first. */
  function take(list: File[], folder: string | null): void {
    if (!leaveEdits()) return;
    if (list.some((f) => RECORDING.test(f.name)) || (list.length > 0 && list.every((f) => SIDECAR.test(f.name)))) openFiles(list);
    else if (list.some((f) => CAPTURE.test(f.name))) choose(list, folder);
    else {
      reset();
      say("There's nothing here to open or convert. Open a .tickwave recording, or convert a capture: a Saleae export's folder, or a sigrok .sr or .vcd file.");
    }
  }

  /** One recording, and the .toml and .jsonl files of its sidecar. */
  function openFiles(all: File[]): void {
    const sidecar = all.filter((f) => SIDECAR.test(f.name));
    const recordings = all.filter((f) => !sidecar.includes(f));
    if (recordings.length !== 1) {
      reset();
      say(recordings.length === 0 ? "Choose a .tickwave recording too: the sidecar's files alone have nothing to draw." : `Choose one recording at a time; these are ${recordings.length}: ${recordings.map((f) => f.name).join(", ")}.`);
      return;
    }
    const hand = sidecar.filter((f) => /\.toml$/i.test(f.name));
    void open(registerFile(recordings[0], sidecar), null, recordings[0].name, false, undefined, null, null, hand.length === 1 ? hand[0] : null);
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
    rows.replaceChildren(
      ...channels.map((c) => {
        const tr = document.createElement("tr");
        tr.dataset.source = c.name;
        const take = Object.assign(document.createElement("input"), { type: "checkbox", checked: true, className: "take" });
        take.setAttribute("aria-label", `Take ${c.name}`);
        take.addEventListener("change", showTaken);
        const where = cell(c.name, " ", Object.assign(document.createElement("span"), {
          className: "quiet",
          textContent: c.analog ? `analog${c.count === null ? "" : `, ${count(c.count)} samples`}` : `digital${c.count === null ? "" : `, ${count(c.count)} edges`}`,
        }));
        const shape = [field("offset", `Move ${c.name} by`, "", 9), field("from", `Keep ${c.name} from`, "", 11), field("to", `Keep ${c.name} to`, "", 11)];
        for (const f of shape) f.addEventListener("change", () => shapeRow(tr));
        tr.append(cell(take), where, cell(field("name", `Name for ${c.name}`, c.name)), cell(...(c.analog ? [field("unit", `Unit for ${c.name}`, c.unit ?? "", 4)] : [])), ...shape.map((f) => cell(f)));
        return tr;
      }),
    );
    skippedBox.textContent = skipped.length ? `Left out: ${skipped.join("; ")}.` : "";
    skippedBox.hidden = skipped.length === 0;
    rateRow.hidden = !described.rateMissing;
    rateField.value = "";
    return true;
  }

  const input = (tr: Element, cls: string) => tr.querySelector<HTMLInputElement>(`input.${cls}`);
  const rowsOf = () => [...rows.children] as HTMLTableRowElement[];

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
      rows.append(...ordered, ...rowsOf().filter((tr) => !ordered.includes(tr)));
    }
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
    const meta: [string, string][] = [];
    for (const line of byId<HTMLTextAreaElement>("meta").value.split("\n")) {
      if (!line.trim()) continue;
      const at = line.indexOf("=");
      const [k, v] = at < 0 ? ["", ""] : [line.slice(0, at).trim(), line.slice(at + 1).trim()];
      if (!k || !v) return `Each metadata line is key = value; this one isn't: ${line.trim()}`;
      meta.push([k, v]);
    }
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

  const label = (pass: Pass, channel: string) =>
    ({
      check: "Checking the export's edges",
      codes: `Looking for exact codes in ${channel}`,
      encode: "Encoding",
      verify: "Checking the recording against the capture",
    })[pass];

  function run(chosen: Settings): void {
    clear();
    settingsForm.inert = true;
    running.hidden = false;
    byId("pass").textContent = "Starting";
    byId("count").textContent = "";
    bar.removeAttribute("value");
    let total = 0;
    let done = 0;
    job({ op: "convert", kind, files, settings: chosen }, (m) => {
      switch (m.op) {
        case "start":
          byId("pass").textContent = label(m.pass, m.channel);
          total = m.total;
          done = 0;
          bar.max = Math.max(total, 1);
          bar.value = 0;
          byId("count").textContent = "";
          break;
        case "step":
          done += m.n;
          bar.value = done;
          byId("count").textContent = `${count(done)} of ${count(total)}`;
          break;
        case "converted":
          end();
          idle();
          converted(new File([m.file], `${stem}.tickwave`), m.codes, m.seconds, chosen);
          break;
        case "failed":
          end();
          idle();
          say(`The conversion failed: ${m.message}`);
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
  function conversionFacts(file: File, codes: [string, number | null][], seconds: number): void {
    const exact = codes.filter(([, bits]) => bits !== null).map(([n, bits]) => `${n} (${bits}-bit)`);
    const f32 = codes.filter(([, bits]) => bits === null).map(([n]) => n);
    byId("took").textContent = `${formatBytes(file.size)}, converted and checked in ${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} s`;
    found.textContent = [exact.length ? `Exact codes: ${exact.join(", ")}.` : "", f32.length ? `Stored as f32: ${f32.join(", ")}.` : ""].filter(Boolean).join(" ");
  }

  /** The capture in the view, shaped by the table: moved, trimmed and reordered as it says. */
  function shapeCapture(): void {
    const c = capture!;
    result = null;
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

  /** The recording saved, as a recording opened: annotations can be edited on it. */
  function showSaved(file: File, codes: [string, number | null][], seconds: number): void {
    result = file;
    void open(registerFile(file, []), null, file.name, false, () => {
      settingsForm.hidden = true;
      reshape.hidden = false;
      save.textContent = `Save ${file.name}`;
      conversionFacts(file, codes, seconds);
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

  /** Save: what the page shows when it is the recording to keep, else a conversion with the
   * table's settings first. */
  save.addEventListener("click", () => {
    if (result) {
      download(result);
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
  reshape.addEventListener("click", () => shapeCapture());

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
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("over");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    if (e.dataTransfer) void dropped(e.dataTransfer).then(({ files, folder }) => take(files, folder));
  });

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
