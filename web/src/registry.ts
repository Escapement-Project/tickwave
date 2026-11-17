// Shared state across every element on the page: one Source and one annotation list per URL.
// What reads a URL is up to the loaders registered here: tickwave.js registers one for .tickwave
// files; the dev page registers one for its fixtures.

import { NONE_DECLARED, type Annotation, type Declared, type Sidecar } from "./annotations.ts";
import { named, type RecordingInfo, type Source } from "./source.ts";

/** Bundled annotations as far as they are read: a recording's parts are read a range at a time
 * (FORMAT.md, "Annotations"). */
export interface BundledRead {
  readonly annotations: Annotation[];
  /** What reading them left out, and why. */
  readonly problems: readonly string[];
  /** Whether every one is read. */
  readonly complete: boolean;
  /** The roles and the stored view the bundle declares, whole before any part is read; none
   * where a loader gives none. */
  readonly declared?: Declared;
}

/** A loader's answer as a sidecar: a list alone declares nothing. */
function sidecarOf(got: Annotation[] | Sidecar): Sidecar {
  return Array.isArray(got) ? { annotations: got, ...NONE_DECLARED } : got;
}

export interface Loader {
  /** Whether this loader reads the recording at `url`. */
  accepts(url: URL): boolean;
  source(url: URL): Source;
  /** The resolved annotations of the sidecar at `annotations`, for the recording at `recording`,
   * with what the sidecar declares beside them where the loader reads that. */
  annotations?(annotations: URL, recording: URL, source: Source): Promise<Annotation[] | Sidecar>;
  /** The annotations bundled into the recording itself, or null when it carries none. */
  bundled?(recording: URL, source: Source): Promise<Annotation[] | Sidecar | null>;
  /** Those of the bundled annotations that ticks `span` meet, with any read before; every one
   * where `span` is null. A loader without it serves `bundled`, whole. */
  bundledIn?(recording: URL, source: Source, span: readonly [number, number] | null): Promise<BundledRead | null>;
  /** The resolved annotations of a sidecar whose hand-written file is `text`, with its includes
   * beside `base` (or the recording's local files when `base` is null). */
  annotationsText?(text: string, base: URL | null, recording: URL, source: Source): Promise<Annotation[] | Sidecar>;
}

const loaders: Loader[] = [];
const sources = new Map<string, Source>();
const infos = new Map<string, Promise<RecordingInfo>>();
const heads = new Map<string, RecordingInfo>();
const annotationLists = new Map<string, Promise<Sidecar | null>>();
const waiting: (() => void)[] = [];

/** Fires `tickwave-head` with the URL once a recording's head is here. */
export const events = new EventTarget();

export function absolute(url: string, base: string = document.baseURI): URL {
  return new URL(url, base);
}

/** Loaders registered later are asked first. */
export function registerLoader(loader: Loader): void {
  loaders.unshift(loader);
  for (const w of waiting.splice(0)) w();
}

/** Waits for a loader that accepts `url` while the page loads, since one may register late; once
 * it has loaded, a URL no loader takes is refused rather than left waiting. */
async function loaderFor(url: URL, signal?: AbortSignal): Promise<Loader> {
  for (;;) {
    const l = loaders.find((l) => l.accepts(url));
    if (l) return l;
    signal?.throwIfAborted();
    if (document.readyState === "complete") {
      throw named("TickwaveNoLoader", `${url.href}: no loader reads it; tickwave.js reads .tickwave files`);
    }
    await new Promise<void>((resolve) => {
      waiting.push(resolve);
      addEventListener("load", () => resolve(), { once: true });
    });
  }
}

export async function sourceFor(url: URL, signal?: AbortSignal): Promise<Source> {
  const key = url.href;
  if (!sources.has(key)) {
    const s = (await loaderFor(url, signal)).source(url);
    if (!sources.has(key)) sources.set(key, s);
  }
  return sources.get(key)!;
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
        events.dispatchEvent(new CustomEvent("tickwave-head", { detail: key }));
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

function cached(key: string, load: () => Promise<Sidecar | null>): Promise<Sidecar | null> {
  let p = annotationLists.get(key);
  if (!p) {
    p = load();
    annotationLists.set(key, p);
    p.catch(() => annotationLists.delete(key));
  }
  return p;
}

/** The sidecar at `annotations`, resolved against the recording at `recording`. */
export function annotationsFor(annotations: URL, recording: URL): Promise<Sidecar> {
  return cached(`${annotations.href}\n${recording.href}`, async () => {
    const loader = await loaderFor(recording);
    if (!loader.annotations) throw new Error(`${recording.pathname}: no reader for its annotations`);
    return sidecarOf(await loader.annotations(annotations, recording, await sourceFor(recording)));
  }) as Promise<Sidecar>;
}

/** The read of the sidecar at `annotations` for the recording at `recording` that `annotationsFor`
 * has started, settled or not; null where none has, and none is started. */
export function annotationsStarted(annotations: URL, recording: URL): Promise<Sidecar> | null {
  return (annotationLists.get(`${annotations.href}\n${recording.href}`) as Promise<Sidecar> | undefined) ?? null;
}

/** The sidecar whose hand-written file is `text`, resolved against the recording at `recording`.
 * Not cached: an editor hands in a new text with each change. */
export async function annotationsFromText(text: string, base: URL | null, recording: URL): Promise<Sidecar> {
  const loader = await loaderFor(recording);
  if (!loader.annotationsText) throw new Error(`${recording.pathname}: no reader for its annotations as text`);
  return sidecarOf(await loader.annotationsText(text, base, recording, await sourceFor(recording)));
}

/** The annotations bundled into the recording, or null. */
export function bundledFor(recording: URL): Promise<Sidecar | null> {
  return cached(`\n${recording.href}`, async () => {
    const loader = await loaderFor(recording);
    const got = loader.bundled ? await loader.bundled(recording, await sourceFor(recording)) : null;
    return got && sidecarOf(got);
  });
}

/** The bundled annotations ticks `span` meet, with any read before, or every one where `span` is
 * null; null where the recording bundles none. Not cached here: the loader keeps what it reads. */
export async function bundledIn(recording: URL, span: readonly [number, number] | null): Promise<BundledRead | null> {
  const loader = await loaderFor(recording);
  if (loader.bundledIn) return loader.bundledIn(recording, await sourceFor(recording), span);
  const whole = await bundledFor(recording);
  return whole && { annotations: whole.annotations, problems: [], complete: true, declared: whole };
}

/** A span no annotation part meets: a read of a bundled sidecar's annotations for it reads the
 * bundle alone, the hand-written file and its generator files' first lines. */
export const BUNDLE_ONLY: readonly [number, number] = [0, 0];

/** The head and the sidecar, ahead of a view: `<tickwave-ref>` calls this when it scrolls into
 * view. Of a bundled sidecar, the bundle alone: its generated annotations wait in the recording's
 * parts until something reads their span. */
export async function prefetch(src: URL, annotations: URL | null): Promise<void> {
  const s = await sourceFor(src);
  await s.prefetch();
  await Promise.allSettled([infoFor(src), annotations ? annotationsFor(annotations, src) : bundledIn(src, BUNDLE_ONLY)]);
}
