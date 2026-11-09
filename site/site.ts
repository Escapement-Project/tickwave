// tickwave's site: the face switch on every page, and the viewer on index.html. Built by
// web/scripts/site.ts; "tickwave" is the tickwave.js beside this script.

import { recordingInfo, registerFile, type RecordingInfo } from "tickwave";
import { formatBytes, formatInterval } from "../web/src/readouts.ts";

// The face: day or night, remembered under the key the project's other sites use.
const face = document.getElementById("face") as HTMLButtonElement;
const shown = () => (getComputedStyle(document.documentElement).colorScheme === "dark" ? "night" : "day");
const labelFace = () => {
  const night = shown() === "night";
  face.textContent = night ? "Day" : "Night";
  face.setAttribute("aria-pressed", String(night));
};
face.addEventListener("click", () => {
  const next = shown() === "night" ? "day" : "night";
  document.documentElement.dataset.face = next;
  try {
    localStorage.setItem("face", next);
  } catch {
    // The choice lasts this visit only.
  }
  labelFace();
});
labelFace();

if (document.body.classList.contains("viewer")) viewer();

function viewer(): void {
  const form = document.getElementById("by-url") as HTMLFormElement;
  const srcField = form.elements.namedItem("src") as HTMLInputElement;
  const sidecarField = form.elements.namedItem("annotations") as HTMLInputElement;
  const files = document.getElementById("files") as HTMLInputElement;
  const drop = document.getElementById("drop")!;
  const problem = document.getElementById("problem")!;
  const stage = document.getElementById("stage")!;
  const facts = document.getElementById("facts")!;
  const share = document.getElementById("share")!;
  const here = document.getElementById("view")!;
  let opened = 0;

  const say = (lead: string, items: string[] = []) => {
    problem.replaceChildren();
    const p = document.createElement("p");
    p.textContent = lead;
    problem.append(p);
    if (items.length > 0) {
      const ul = document.createElement("ul");
      for (const item of items) ul.append(Object.assign(document.createElement("li"), { textContent: item }));
      problem.append(ul);
    }
    problem.hidden = false;
  };
  const clear = () => {
    problem.hidden = true;
    problem.replaceChildren();
  };

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
    showFacts(info, name);
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

  function showFacts(info: RecordingInfo, name: string): void {
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

/** A failure, in plain words. */
function explain(e: unknown, src: string, name: string): string {
  const error = e instanceof Error ? e : new Error(String(e));
  const url = new URL(src, location.href);
  switch (error.name) {
    case "TickwaveNotRecording":
      return `${name} isn't a tickwave recording. The viewer reads .tickwave files, which the converter makes from a capture.`;
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
