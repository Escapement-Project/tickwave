// tickwave's site: the viewer on index.html and the converter on convert.html. Built by
// web/scripts/site.ts; "tickwave" is the tickwave.js beside this script. The project's face.js and
// header.js run the header.

import { recordingInfo, registerFile, type RecordingInfo } from "tickwave";
import { formatBytes, formatInterval } from "../web/src/readouts.ts";
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
  const source = info.source as { format?: string; files?: string[]; converter?: string } | undefined;
  if (source?.format) rows.push(["From", [source.format, ...(source.files ?? [])].join(", ")]);
  if (source?.converter) rows.push(["Converted by", source.converter]);
  for (const [k, v] of Object.entries(info.meta ?? {})) rows.push([k, v]);
  if (info.contentId) rows.push(["Content id", info.contentId]);
  facts.replaceChildren(...rows.flatMap(([k, v]) => [Object.assign(document.createElement("dt"), { textContent: k }), Object.assign(document.createElement("dd"), { textContent: v })]));
}

function viewer(): void {
  const form = document.getElementById("by-url") as HTMLFormElement;
  const srcField = form.elements.namedItem("src") as HTMLInputElement;
  const sidecarField = form.elements.namedItem("annotations") as HTMLInputElement;
  const files = document.getElementById("files") as HTMLInputElement;
  const drop = document.getElementById("drop")!;
  const stage = document.getElementById("stage")!;
  const facts = document.getElementById("facts")!;
  const share = document.getElementById("share")!;
  const here = document.getElementById("view")!;
  const opener = byId<HTMLDetailsElement>("opener");
  const { say, clear } = problems(byId("problem"));
  let opened = 0;

  /** Opens `src`, with its sidecar at `annotations` or a local recording's own. */
  async function open(src: string, annotations: string | null, name: string, link: boolean): Promise<void> {
    const token = ++opened;
    clear();
    stage.hidden = true;
    here.replaceChildren();
    let info: RecordingInfo;
    try {
      info = await recordingInfo(src);
    } catch (e) {
      if (token === opened) say(explain(e, src, name));
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
    stage.hidden = false;
    // The recording, not the forms that asked for it: they fold away, and once the view has drawn
    // it comes into sight with the focus, so its keys work at once.
    opener.classList.add("folds");
    opener.open = false;
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
      const q = new URLSearchParams({ src, ...(annotations && { annotations }) });
      history.replaceState(null, "", `?${q}`);
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

  /** One recording, and the .toml and .jsonl files of its sidecar. */
  function openFiles(list: FileList | File[]): void {
    const all = [...list];
    const sidecar = all.filter((f) => /\.(toml|jsonl)$/i.test(f.name));
    const recordings = all.filter((f) => !sidecar.includes(f));
    if (recordings.length !== 1) {
      say(recordings.length === 0 ? "Choose a .tickwave recording too: the sidecar's files alone have nothing to draw." : `Choose one recording at a time; these are ${recordings.length}: ${recordings.map((f) => f.name).join(", ")}.`);
      return;
    }
    void open(registerFile(recordings[0], sidecar), null, recordings[0].name, false);
  }

  files.addEventListener("change", () => {
    if (files.files && files.files.length > 0) openFiles(files.files);
  });
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("over");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    if (e.dataTransfer?.files.length) openFiles(e.dataTransfer.files);
  });

  const q = new URLSearchParams(location.search);
  const src = q.get("src");
  if (src) {
    srcField.value = src;
    sidecarField.value = q.get("annotations") ?? "";
    void open(src, q.get("annotations"), src, true);
  }
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

function converter(): void {
  const drop = byId("drop");
  const folderInput = byId<HTMLInputElement>("folder");
  const fileInput = byId<HTMLInputElement>("file");
  const form = byId<HTMLFormElement>("settings");
  const rows = byId("channels");
  const skippedBox = byId("skipped");
  const running = byId("running");
  const bar = byId<HTMLProgressElement>("bar");
  const result = byId("result");
  const save = byId<HTMLAnchorElement>("save");
  const view = byId("view");
  const { say, clear } = problems(byId("problem"));

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
  const idle = () => {
    running.hidden = true;
    form.inert = false;
  };

  function choose(list: File[], folder: string | null): void {
    end();
    clear();
    idle();
    form.hidden = true;
    result.hidden = true;
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
    } else if (bin.length > 0) {
      kind = "saleae";
      files = bin;
      stem = folder ?? "capture";
    } else {
      say("There's no capture here. The converter takes a Saleae export's .bin files, a sigrok .sr file or a .vcd file.");
      return;
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
    form.hidden = false;
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
    result.hidden = true;
    form.inert = true;
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
          void converted(new File([m.file], `${stem}.tickwave`), m.codes, m.seconds);
          break;
        case "failed":
          end();
          idle();
          say(`The conversion failed: ${m.message}`);
          break;
      }
    });
  }

  async function converted(file: File, codes: [string, number | null][], seconds: number): Promise<void> {
    if (saved) URL.revokeObjectURL(saved);
    saved = URL.createObjectURL(file);
    save.href = saved;
    save.download = file.name;
    save.textContent = `Save ${file.name}`;
    byId("took").textContent = `${formatBytes(file.size)}, converted and checked in ${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} s`;
    const exact = codes.filter(([, bits]) => bits !== null).map(([n, bits]) => `${n} (${bits}-bit)`);
    const f32 = codes.filter(([, bits]) => bits === null).map(([n]) => n);
    byId("found").textContent = [exact.length ? `Exact codes: ${exact.join(", ")}.` : "", f32.length ? `Stored as f32: ${f32.join(", ")}.` : ""].filter(Boolean).join(" ");
    view.replaceChildren();
    result.hidden = false;
    // The next thing to do is save it: the focus goes there, and brings it into sight.
    save.focus();
    const src = registerFile(file, []);
    try {
      showFacts(byId("facts"), await recordingInfo(src), file.name);
    } catch (e) {
      say(`The recording was made, but it won't open: ${e instanceof Error ? e.message : e}`);
      return;
    }
    const v = document.createElement("tickwave-view");
    v.setAttribute("src", src);
    v.setAttribute("open", "");
    view.append(v);
  }

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    const chosen = settings();
    if (typeof chosen === "string") say(chosen);
    else run(chosen);
  });
  byId("cancel").addEventListener("click", () => {
    end();
    idle();
  });
  folderInput.addEventListener("change", () => {
    const list = [...(folderInput.files ?? [])];
    // Only the folder's own files: an export keeps none in folders of its own.
    const own = list.filter((f) => f.webkitRelativePath.split("/").length === 2);
    choose(own, own[0]?.webkitRelativePath.split("/")[0] ?? null);
    folderInput.value = "";
  });
  fileInput.addEventListener("change", () => {
    choose([...(fileInput.files ?? [])], null);
    fileInput.value = "";
  });
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("over");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    if (e.dataTransfer) void dropped(e.dataTransfer).then(({ files, folder }) => choose(files, folder));
  });
}

/** A failure, in plain words. */
function explain(e: unknown, src: string, name: string): string {
  const error = e instanceof Error ? e : new Error(String(e));
  const url = new URL(src, location.href);
  switch (error.name) {
    case "TickwaveNotRecording":
      return `${name} isn't a tickwave recording. The viewer reads .tickwave files, which the converter makes from a capture.`;
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
if (document.body.classList.contains("viewer")) viewer();
if (document.body.classList.contains("convert")) converter();
