// tickwave's site: the Open page, where a recording opens, or a capture converts and opens. Built by
// web/scripts/site.ts; "tickwave" is the tickwave.js beside this script. The project's face.js and
// header.js run the header.

import { recordingInfo, registerFile, type RecordingInfo, type TickwaveView } from "tickwave";
import { parseWindow } from "../web/src/attributes.ts";
import { formatBytes, formatInterval } from "../web/src/readouts.ts";
import { toTicks } from "../web/src/time.ts";
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

/** A recording's facts as a definition list. */
function showFacts(facts: HTMLElement, info: RecordingInfo, name: string): void {
  const rows: [string, string][] = [
    ["Recording", info.title || name],
    ["Length", formatInterval(info.length, info.timebase)],
    ["One tick", formatInterval(1, info.timebase)],
    ["Channels", info.channels.map((c) => (c.kind === "analog" ? `${c.name} (analog, ${c.unit})` : c.name)).join(", ")],
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
  const save = byId<HTMLAnchorElement>("save");
  const found = byId("found");
  const facts = byId("facts");
  const share = byId("share");
  const here = byId("view");
  const { say, clear } = problems(byId("problem"));
  /** Served by `tickwave open`, which names the recording by an address that lasts as long as it runs. */
  const local = new URLSearchParams(location.search).has("local");

  /** Counts the recordings asked for: one asked for later makes an earlier one's answer stale. */
  let opened = 0;
  let kind: SourceKind = "saleae";
  let files: File[] = [];
  /** The recording's name, without .tickwave: the export's folder, or the capture file's. */
  let stem = "capture";
  let channels: ChannelInfo[] = [];
  let worker: Worker | null = null;
  let saved: string | null = null;

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
    converted?: () => void,
    at: string | null = null,
    span: string | null = null,
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
    const spec = parseWindow(span);
    const ticks = spec?.kind === "span" ? { start: toTicks(spec.start, info.timebase), end: toTicks(spec.end, info.timebase) } : null;
    if (at) void (view as TickwaveView).show(at, ticks);
    else if (span) view.setAttribute("window", span);
    saving.hidden = found.hidden = !converted;
    stage.hidden = false;
    // The recording, not the forms that asked for it: they fold away.
    opener.classList.add("folds");
    opener.open = false;
    if (converted) converted();
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
      const q = new URLSearchParams({ src, ...(annotations && { annotations }), ...(at && { annotation: at }), ...(span && { window: span }) });
      history.replaceState(null, "", `?${q}${local ? "&local" : ""}`);
      share.textContent = "";
      share.append("A link to this view: ", Object.assign(document.createElement("a"), { href: location.href, textContent: location.href }));
    } else {
      history.replaceState(null, "", location.pathname);
      share.textContent = "Files from this computer can't be shared by link.";
    }
  }

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    const src = srcField.value.trim();
    const annotations = sidecarField.value.trim() || null;
    if (src) void open(src, annotations, src, true);
  });

  /** What was chosen or dropped: a recording opens, a capture converts first. */
  function take(list: File[], folder: string | null): void {
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
    void open(registerFile(recordings[0], sidecar), null, recordings[0].name, false);
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
        show(m.described);
      } else if (m.op === "failed") {
        end();
        say(`${kind === "saleae" ? "The export" : files[0].name} couldn't be read: ${m.message}`);
      }
    });
  }

  function show(described: Described): void {
    channels = described.channels;
    const what = kind === "saleae" ? "a Saleae Logic 2 binary export" : kind === "sigrok" ? "a sigrok session" : "a value change dump";
    byId("source").textContent = kind === "saleae" ? stem : files[0].name;
    byId("source-facts").textContent = `${what}, with ${channels.length === 1 ? "one channel" : `${channels.length} channels`} tickwave can take.`;
    const skipped = described.skipped.map((s) => `${s.name}: ${s.reason}`);
    if (channels.length === 0) {
      say(`${byId("source").textContent} has no channels tickwave can take.`, skipped);
      return;
    }
    rows.replaceChildren(
      ...channels.map((c) => {
        const tr = document.createElement("tr");
        const take = Object.assign(document.createElement("input"), { type: "checkbox", checked: true });
        take.setAttribute("aria-label", `Take ${c.name}`);
        const name = Object.assign(document.createElement("input"), { type: "text", value: c.name, spellcheck: false });
        name.setAttribute("aria-label", `Name for ${c.name}`);
        const where = document.createElement("td");
        where.append(c.name, " ", Object.assign(document.createElement("span"), {
          className: "quiet",
          textContent: c.analog ? `analog${c.count === null ? "" : `, ${count(c.count)} samples`}` : `digital${c.count === null ? "" : `, ${count(c.count)} edges`}`,
        }));
        const unit = document.createElement("td");
        if (c.analog) {
          const field = Object.assign(document.createElement("input"), { type: "text", value: c.unit ?? "", spellcheck: false, size: 4 });
          field.setAttribute("aria-label", `Unit for ${c.name}`);
          unit.append(field);
        }
        const cells = [take, name].map((el) => {
          const td = document.createElement("td");
          td.append(el);
          return td;
        });
        tr.append(cells[0], where, cells[1], unit);
        return tr;
      }),
    );
    skippedBox.textContent = skipped.length ? `Left out: ${skipped.join("; ")}.` : "";
    skippedBox.hidden = skipped.length === 0;
    rateRow.hidden = !described.rateMissing;
    rateField.value = "";
    settingsForm.hidden = false;
  }

  /** The form as the converter's settings, or what is wrong with it. */
  function settings(): Settings | string {
    const taken: [string, string][] = [];
    const units: [string, string][] = [];
    [...rows.children].forEach((tr, i) => {
      const [take, name, unit] = [...tr.querySelectorAll("input")] as HTMLInputElement[];
      if (!take.checked) return;
      const to = name.value.trim();
      taken.push([channels[i].name, to]);
      const u = unit?.value.trim() ?? "";
      if (unit && u && u !== (channels[i].unit ?? "")) units.push([to, u]);
    });
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
          converted(new File([m.file], `${stem}.tickwave`), m.codes, m.seconds);
          break;
        case "failed":
          end();
          idle();
          say(`The conversion failed: ${m.message}`);
          break;
      }
    });
  }

  function converted(file: File, codes: [string, number | null][], seconds: number): void {
    if (saved) URL.revokeObjectURL(saved);
    saved = URL.createObjectURL(file);
    const exact = codes.filter(([, bits]) => bits !== null).map(([n, bits]) => `${n} (${bits}-bit)`);
    const f32 = codes.filter(([, bits]) => bits === null).map(([n]) => n);
    void open(registerFile(file, []), null, file.name, false, () => {
      save.href = saved!;
      save.download = file.name;
      save.textContent = `Save ${file.name}`;
      byId("took").textContent = `${formatBytes(file.size)}, converted and checked in ${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} s`;
      found.textContent = [exact.length ? `Exact codes: ${exact.join(", ")}.` : "", f32.length ? `Stored as f32: ${f32.join(", ")}.` : ""].filter(Boolean).join(" ");
      // The next thing to do is save it: the focus goes there, and brings it into sight.
      save.focus();
    });
  }

  settingsForm.addEventListener("submit", (e) => {
    e.preventDefault();
    const chosen = settings();
    if (typeof chosen === "string") say(chosen);
    else run(chosen);
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
