// The page's side of the decoder: a Source for each .tickwave recording, answered by one worker that
// holds the WASM module. The worker starts when something first needs a recording.

import { declaredFromCore, fromCore, NONE_DECLARED, type CoreAnnotation, type Sidecar, type StoredView } from "../annotations.ts";
import type { Role } from "../roles.ts";
import type { PeriodStats } from "../readouts.ts";
import type { BundledRead, Loader } from "../registry.ts";
import { aborted, type Edges, type Overview, type RecordingInfo, type Samples, type Source } from "../source.ts";
import type { AnnotationsRead } from "./opened.ts";
import { FIRST_READ, MAX_DECODED, type Reply, type Request } from "./protocol.ts";

const settings = { firstRead: FIRST_READ, maxDecodedBytes: MAX_DECODED, cacheBytes: 192 << 20 };

/** Tuning for the decoder: the first read of a recording's head, the most it decodes from one
 * block, and the decoded-block cache. A recording opened earlier keeps the settings it opened with. */
export function configure(options: { firstRead?: number; maxDecodedBytes?: number; cacheBytes?: number }): void {
  Object.assign(settings, options);
}

let worker: Worker | null = null;
let nextId = 1;
const pending = new Map<number, { resolve(v: unknown): void; reject(e: unknown): void }>();

/**
 * A browser starts a worker only from the page's own origin. When this module came from another
 * one, as from a release's directory on tickwave's site, the worker is a script of the page's own
 * that imports the real one, which that server must send with CORS. The script is a classic one:
 * on a page opened from disk, Chromium refuses a module worker made from a blob. It holds every
 * message that comes before the import has run, kept from any handler the real one sets meanwhile,
 * then hands them on in order. An import that fails is thrown again outside its promise, so the
 * page's `error` handler hears of it. The real one finds its WASM beside itself, by its own URL.
 * The script's blob URL is never revoked: not every engine is known to keep it for a worker that
 * is still starting.
 */
function startWorker(name: string): Worker {
  const url = new URL("./tickwave-worker.js", import.meta.url);
  if (url.origin === location.origin) return new Worker(url, { type: "module", name });
  const script = `const held = [];
const hold = (e) => (e.stopImmediatePropagation(), held.push(e));
self.addEventListener("message", hold);
import(${JSON.stringify(url.href)}).then(
  () => {
    self.removeEventListener("message", hold);
    for (const e of held) self.dispatchEvent(new MessageEvent("message", { data: e.data, ports: e.ports }));
  },
  (e) => setTimeout(() => { throw e; }),
);`;
  return new Worker(URL.createObjectURL(new Blob([script], { type: "text/javascript" })), { name });
}

function start(): Worker {
  if (worker) return worker;
  const w = startWorker("tickwave");
  w.onmessage = (e: MessageEvent<Reply>) => {
    const r = e.data;
    const p = pending.get(r.id);
    if (!p) return;
    pending.delete(r.id);
    if (r.ok) p.resolve(r.value);
    else p.reject(r.name === "AbortError" ? aborted() : Object.assign(new Error(r.message), { name: r.name }));
  };
  w.onerror = (e) => {
    const error = new Error(`tickwave's decoder failed to start: ${e.message || "see the console"}`);
    for (const p of pending.values()) p.reject(error);
    pending.clear();
    worker = null;
  };
  return (worker = w);
}

type Body = Request extends infer R ? (R extends { id: number } ? Omit<R, "id"> : never) : never;

/**
 * The requests not yet posted. The first queues a microtask that posts it, with every request made
 * before that microtask runs, in one message: the worker starts them in one task of its own, so
 * the reads they need, a view's channels' at once, go out together. A request made after it runs
 * goes in the next message.
 */
let batch: Request[] | null = null;

function post(w: Worker, m: Request): void {
  if (!batch) {
    const b: Request[] = (batch = []);
    queueMicrotask(() => {
      batch = null;
      if (b.length > 0) w.postMessage(b.length === 1 ? b[0] : { op: "batch", requests: b });
    });
  }
  batch.push(m);
}

function call<T>(body: Body, signal?: AbortSignal): Promise<T> {
  if (signal?.aborted) return Promise.reject(aborted());
  const w = start();
  const id = nextId++;
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => {
      if (!pending.delete(id)) return;
      // Not posted yet: it goes no further. Else the worker stops it.
      const at = batch?.findIndex((m) => m.id === id) ?? -1;
      if (at >= 0) batch!.splice(at, 1);
      else w.postMessage({ op: "abort", id });
      reject(aborted());
    };
    pending.set(id, {
      resolve: (v) => {
        signal?.removeEventListener("abort", onAbort);
        resolve(v as T);
      },
      reject: (e) => {
        signal?.removeEventListener("abort", onAbort);
        reject(e);
      },
    });
    signal?.addEventListener("abort", onAbort, { once: true });
    post(w, { ...body, id } as Request);
  });
}

export class WasmSource implements Source {
  readonly #key: string;
  readonly #url: string | null;
  readonly #file: File | null;
  readonly #sidecar: readonly File[];
  #info: Promise<RecordingInfo> | null = null;

  constructor(key: string, target: { url: string } | { file: File; sidecar?: readonly File[] }) {
    this.#key = key;
    this.#url = "url" in target ? target.url : null;
    this.#file = "file" in target ? target.file : null;
    this.#sidecar = "file" in target ? (target.sidecar ?? []) : [];
  }

  /** The recording's file name, for the sidecar's `recording` check. */
  get fileName(): string | null {
    if (this.#file) return this.#file.name;
    const last = new URL(this.#url!).pathname.split("/").pop();
    return last ? decodeURIComponent(last) : null;
  }

  /** Opens the recording in the worker once; a failed open is tried again on the next call. */
  #open(): Promise<RecordingInfo> {
    this.#info ??= call<RecordingInfo>({
      op: "open",
      key: this.#key,
      ...(this.#url !== null && { url: this.#url }),
      ...(this.#file !== null && { file: this.#file }),
      firstRead: settings.firstRead,
      maxDecoded: settings.maxDecodedBytes,
      cacheBytes: settings.cacheBytes,
    });
    this.#info.catch(() => (this.#info = null));
    return this.#info;
  }

  info(signal?: AbortSignal): Promise<RecordingInfo> {
    return signal ? Promise.race([this.#open(), abortOf(signal)]) : this.#open();
  }

  prefetch(): Promise<void> {
    return this.#open().then(
      () => undefined,
      () => undefined,
    );
  }

  async edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges> {
    await this.#open();
    return call<Edges>({ op: "edges", key: this.#key, channel, start, end }, signal);
  }

  async edgeCount(channel: number, start: number, end: number, signal?: AbortSignal): Promise<number> {
    await this.#open();
    return call<number>({ op: "edgeCount", key: this.#key, channel, start, end }, signal);
  }

  async periods(channel: number, start: number, end: number, signal?: AbortSignal): Promise<PeriodStats | null> {
    await this.#open();
    return call<PeriodStats | null>({ op: "periods", key: this.#key, channel, start, end }, signal);
  }

  async samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    await this.#open();
    return call<Samples>({ op: "samples", key: this.#key, channel, start, end }, signal);
  }

  async overview(channel: number, start: number, end: number, maxBucket: number, signal?: AbortSignal): Promise<Overview | null> {
    await this.#open();
    return call<Overview | null>({ op: "overview", key: this.#key, channel, start, end, maxBucket }, signal);
  }

  async sidecar(url: URL): Promise<Sidecar> {
    await this.#open();
    const { json, names } = await call<{ json: string; names: string[] }>({ op: "sidecar", key: this.#key, url: url.href, recordingFile: this.fileName });
    return { ...sidecarFromCore(json), files: names };
  }

  /** The sidecar whose hand-written file is `text`. Its includes are beside `base`, or among the
   * local files that came with the recording when there is no `base`. */
  async sidecarText(text: string, base: URL | null): Promise<Sidecar> {
    await this.#open();
    let json: string;
    if (base === null && this.#sidecar.length > 0) {
      const rest = this.#sidecar.filter((f) => !f.name.endsWith(".toml"));
      const texts = await Promise.all(rest.map((f) => f.text()));
      json = await call<string>({ op: "resolve", key: this.#key, hand: text, names: rest.map((f) => f.name), texts, recordingFile: this.fileName });
    } else json = await call<string>({ op: "sidecarText", key: this.#key, hand: text, base: base?.href ?? null, recordingFile: this.fileName });
    return sidecarFromCore(json);
  }

  /** A local file's sidecar files when it came with them, else the recording's bundled sidecar,
   * whole. */
  async bundled(): Promise<Sidecar | null> {
    const read = await this.bundledIn(null);
    return read && { annotations: read.annotations, ...(read.declared ?? NONE_DECLARED), notes: read.notes };
  }

  /** A local file's sidecar files when it came with them, whole; else the recording's bundled
   * annotations the parts meeting ticks `span` hold, with those read before, every part's where
   * `span` is null. */
  async bundledIn(span: readonly [number, number] | null): Promise<BundledRead | null> {
    await this.#open();
    if (this.#sidecar.length > 0) {
      const hand = this.#sidecar.filter((f) => f.name.endsWith(".toml"));
      if (hand.length !== 1) throw new Error(`a sidecar needs one .toml file; ${hand.length} came with ${this.fileName}`);
      const rest = this.#sidecar.filter((f) => f !== hand[0]);
      const [text, ...texts] = await Promise.all([hand[0].text(), ...rest.map((f) => f.text())]);
      const json = await call<string>({ op: "resolve", key: this.#key, hand: text, names: rest.map((f) => f.name), texts, recordingFile: this.fileName });
      const whole = sidecarFromCore(json);
      return { annotations: whole.annotations, problems: [], notes: whole.notes, complete: true, declared: whole };
    }
    const read = await call<AnnotationsRead | null>({ op: "bundled", key: this.#key, span });
    return read && { annotations: read.list.map(fromCore), problems: read.problems, notes: read.notes, complete: read.complete, declared: declaredFromCore(read.roles, read.view) };
  }
}

/** A sidecar as the decoder resolves it whole (`Recording.resolveAnnotations`). */
function sidecarFromCore(json: string): Sidecar {
  const s = JSON.parse(json) as { annotations: CoreAnnotation[]; notes: string[]; roles: Role[]; view: StoredView | null };
  return { annotations: s.annotations.map(fromCore), notes: s.notes, ...declaredFromCore(s.roles, s.view) };
}

function abortOf(signal: AbortSignal): Promise<never> {
  return new Promise((_, reject) => {
    if (signal.aborted) reject(aborted());
    signal.addEventListener("abort", () => reject(aborted()), { once: true });
  });
}

const files = new Map<string, { file: File; sidecar: readonly File[] }>();

/**
 * Makes a local file a `src`: returns a URL the elements read through the file's `slice`. Its
 * sidecar may come with it, the hand-written `.toml` and the files it includes, and serves a view
 * of it that names no `annotations`.
 */
export function registerFile(file: File, sidecar: readonly File[] = []): string {
  const url = URL.createObjectURL(file);
  files.set(url, { file, sidecar });
  return url;
}

/** Whether `url` is a local file registered with the page. */
export function isRegistered(url: URL): boolean {
  return files.has(url.href);
}

/** A registered local file's name, which a download of its blob takes; null for any other URL. */
export function registeredName(url: URL): string | null {
  return files.get(url.href)?.file.name ?? null;
}

/** The sidecar files a registered local file came with, in the order given; none for any other URL. */
export function registeredSidecar(url: URL): readonly File[] {
  return files.get(url.href)?.sidecar ?? [];
}

/** The names a sidecar's hand-written file includes, in its order: the file served at `url`, or
 * `text`, which `where` names in a syntax error. Read by the worker, which holds the TOML parser. */
export function includesOf(from: { url: URL } | { text: string; where: string }): Promise<string[]> {
  return "url" in from
    ? call<string[]>({ op: "includes", url: from.url.href, hand: null, where: "" })
    : call<string[]>({ op: "includes", url: null, hand: from.text, where: from.where });
}

/** Reads `.tickwave` URLs, in any case, and local files. Not every URL: a page may register a loader
 * of its own while it loads, after its elements have asked for recordings. */
export const tickwaveLoader: Loader = {
  accepts: (url) => url.pathname.toLowerCase().endsWith(".tickwave") || files.has(url.href),
  source: (url) => {
    const local = files.get(url.href);
    return new WasmSource(url.href, local ?? { url: url.href });
  },
  annotations: (annotations, _recording, source) => (source as WasmSource).sidecar(annotations),
  annotationsText: (text, base, _recording, source) => (source as WasmSource).sidecarText(text, base),
  bundled: (_recording, source) => (source as WasmSource).bundled(),
  bundledIn: (_recording, source, span) => (source as WasmSource).bundledIn(span),
};
