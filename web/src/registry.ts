// Shared state across every element on the page: one Source and one annotation list per URL.
// What reads a URL is up to the loaders registered here. The WASM decoder will register one for
// .chrono files; the dev page registers one for its fixtures.

import type { RawAnnotation } from "./annotations.ts";
import type { RecordingInfo, Source } from "./source.ts";

export interface Loader {
  /** Whether this loader reads the recording or sidecar at `url`. */
  accepts(url: URL): boolean;
  source(url: URL): Source;
  /** The annotations at `url`, with the sidecar's fields. */
  annotations?(url: URL): Promise<RawAnnotation[]>;
}

const loaders: Loader[] = [];
const sources = new Map<string, Source>();
const infos = new Map<string, Promise<RecordingInfo>>();
const heads = new Map<string, RecordingInfo>();
const annotationLists = new Map<string, Promise<RawAnnotation[]>>();
const waiting: (() => void)[] = [];

/** Fires `chrono-head` with the URL once a recording's head is here. */
export const events = new EventTarget();

export function absolute(url: string, base: string = document.baseURI): URL {
  return new URL(url, base);
}

/** Loaders registered later are asked first. */
export function registerLoader(loader: Loader): void {
  loaders.unshift(loader);
  for (const w of waiting.splice(0)) w();
}

function loaderFor(url: URL, what: "source" | "annotations"): Loader | null {
  return loaders.find((l) => l.accepts(url) && (what === "source" || l.annotations !== undefined)) ?? null;
}

/** Waits for a loader that accepts `url`, if none is registered yet. */
async function waitFor(url: URL, what: "source" | "annotations", signal?: AbortSignal): Promise<Loader> {
  for (;;) {
    const l = loaderFor(url, what);
    if (l) return l;
    signal?.throwIfAborted();
    await new Promise<void>((resolve) => waiting.push(resolve));
  }
}

export async function sourceFor(url: URL, signal?: AbortSignal): Promise<Source> {
  const key = url.href;
  let s = sources.get(key);
  if (!s) {
    s = (await waitFor(url, "source", signal)).source(url);
    if (!sources.has(key)) sources.set(key, s);
    s = sources.get(key)!;
  }
  return s;
}

/** The recording's info, fetched once a URL. */
export function infoFor(url: URL): Promise<RecordingInfo> {
  const key = url.href;
  let p = infos.get(key);
  if (!p) {
    p = sourceFor(url).then((s) => s.info());
    infos.set(key, p);
    p.then(
      (info) => {
        heads.set(key, info);
        events.dispatchEvent(new CustomEvent("chrono-head", { detail: key }));
      },
      () => infos.delete(key),
    );
  }
  return p;
}

/** The info if the head is already here, without fetching. */
export function peekInfo(url: URL): RecordingInfo | null {
  return heads.get(url.href) ?? null;
}

export function annotationsFor(url: URL): Promise<RawAnnotation[]> {
  const key = url.href;
  let p = annotationLists.get(key);
  if (!p) {
    p = waitFor(url, "annotations").then((l) => l.annotations!(url));
    annotationLists.set(key, p);
    p.catch(() => annotationLists.delete(key));
  }
  return p;
}

/** The head and the sidecar, ahead of a view: `<chrono-ref>` calls this when it scrolls into view. */
export async function prefetch(src: URL, annotations: URL | null): Promise<void> {
  const s = await sourceFor(src);
  await s.prefetch();
  await Promise.allSettled([infoFor(src), annotations ? annotationsFor(annotations) : null]);
}
