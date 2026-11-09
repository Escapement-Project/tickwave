// The page's side of the decoder: a Source for each .chrono recording, answered by one worker that
// holds the WASM module. The worker starts when something first needs a recording.

import { fromCore, type Annotation, type CoreAnnotation } from "../annotations.ts";
import type { Loader } from "../registry.ts";
import { aborted, type Edges, type Overview, type RecordingInfo, type Samples, type Source } from "../source.ts";
import type { Reply, Request } from "./protocol.ts";

const settings = { firstRead: 64 * 1024, cacheBytes: 192 << 20 };

/** Tuning for the decoder: the first read of a recording's head, and the decoded-block cache. */
export function configure(options: { firstRead?: number; cacheBytes?: number }): void {
  Object.assign(settings, options);
}

let worker: Worker | null = null;
let nextId = 1;
const pending = new Map<number, { resolve(v: unknown): void; reject(e: unknown): void }>();

function start(): Worker {
  if (worker) return worker;
  const w = new Worker(new URL("./chrono-worker.js", import.meta.url), { type: "module", name: "chrono" });
  w.onmessage = (e: MessageEvent<Reply>) => {
    const r = e.data;
    const p = pending.get(r.id);
    if (!p) return;
    pending.delete(r.id);
    if (r.ok) p.resolve(r.value);
    else p.reject(r.name === "AbortError" ? aborted() : Object.assign(new Error(r.message), { name: r.name }));
  };
  w.onerror = (e) => {
    const error = new Error(`chrono's decoder failed to start: ${e.message || "see the console"}`);
    for (const p of pending.values()) p.reject(error);
    pending.clear();
    worker = null;
  };
  return (worker = w);
}

type Body = Request extends infer R ? (R extends { id: number } ? Omit<R, "id"> : never) : never;

function call<T>(body: Body, signal?: AbortSignal): Promise<T> {
  if (signal?.aborted) return Promise.reject(aborted());
  const w = start();
  const id = nextId++;
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => {
      if (!pending.delete(id)) return;
      w.postMessage({ op: "abort", id });
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
    w.postMessage({ ...body, id });
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

  async samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    await this.#open();
    return call<Samples>({ op: "samples", key: this.#key, channel, start, end }, signal);
  }

  async overview(channel: number, start: number, end: number, maxBucket: number, signal?: AbortSignal): Promise<Overview | null> {
    await this.#open();
    return call<Overview | null>({ op: "overview", key: this.#key, channel, start, end, maxBucket }, signal);
  }

  async sidecar(url: URL): Promise<Annotation[]> {
    await this.#open();
    const json = await call<string>({ op: "sidecar", key: this.#key, url: url.href, recordingFile: this.fileName });
    return (JSON.parse(json) as CoreAnnotation[]).map(fromCore);
  }

  /** A local file's sidecar files when it came with them, else the recording's bundled sidecar. */
  async bundled(): Promise<Annotation[] | null> {
    await this.#open();
    let json: string | null;
    if (this.#sidecar.length > 0) {
      const hand = this.#sidecar.filter((f) => f.name.endsWith(".toml"));
      if (hand.length !== 1) throw new Error(`a sidecar needs one .toml file; ${hand.length} came with ${this.fileName}`);
      const rest = this.#sidecar.filter((f) => f !== hand[0]);
      const [text, ...texts] = await Promise.all([hand[0].text(), ...rest.map((f) => f.text())]);
      json = await call<string>({ op: "resolve", key: this.#key, hand: text, names: rest.map((f) => f.name), texts, recordingFile: this.fileName });
    } else json = await call<string | null>({ op: "bundled", key: this.#key });
    return json === null ? null : (JSON.parse(json) as CoreAnnotation[]).map(fromCore);
  }
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

export const chronoLoader: Loader = {
  accepts: (url) => url.pathname.endsWith(".chrono") || files.has(url.href),
  source: (url) => {
    const local = files.get(url.href);
    return new WasmSource(url.href, local ?? { url: url.href });
  },
  annotations: (annotations, _recording, source) => (source as WasmSource).sidecar(annotations),
  bundled: (_recording, source) => (source as WasmSource).bundled(),
};
