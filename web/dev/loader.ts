// The dev page's loader: recordings from fixture files, annotations from a hand-written JSON file
// and the generator files it includes (JSON Lines, PLAN.md §3). Stands in for the WASM decoder and
// the sidecar parser until they are connected.
//
// A fixture URL may carry ?latency=MS to delay every answer, so the page can show the renderer
// drawing what it has while blocks are on their way.

import { registerLoader, type RawAnnotation, type Source } from "chrono";
import { readFixture } from "../src/fixture-file.ts";
import { MemorySource } from "../src/memory-source.ts";

async function fetchOk(url: URL): Promise<Response> {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url.pathname}: HTTP ${r.status}`);
  return r;
}

/** Fetches the fixture the first time anything asks. */
class FixtureSource implements Source {
  readonly #url: URL;
  #loaded: Promise<MemorySource> | null = null;

  constructor(url: URL) {
    this.#url = url;
  }

  #load(): Promise<MemorySource> {
    this.#loaded ??= fetchOk(this.#url)
      .then((r) => r.arrayBuffer())
      .then(readFixture)
      .then((rec) => new MemorySource(rec, { latency: Number(this.#url.searchParams.get("latency") ?? 0) }));
    return this.#loaded;
  }

  info: Source["info"] = (signal) => this.#load().then((s) => s.info(signal));
  edges: Source["edges"] = (c, s, e, signal) => this.#load().then((m) => m.edges(c, s, e, signal));
  overview: Source["overview"] = (c, s, e, b, signal) => this.#load().then((m) => m.overview(c, s, e, b, signal));
  samples: Source["samples"] = (c, s, e, signal) => this.#load().then((m) => m.samples(c, s, e, signal));
  prefetch = (): Promise<void> => this.#load().then(
    () => undefined,
    () => undefined,
  );
}

function jsonLines(text: string): RawAnnotation[] {
  const lines = text.split("\n").filter((l) => l.trim() !== "");
  const head = JSON.parse(lines[0]) as { format?: string; source?: string };
  if (head.format !== "chrono-annotations/1" || !head.source) throw new Error("not a generator's annotation file");
  return lines.slice(1).map((l) => ({ ...(JSON.parse(l) as RawAnnotation), source: head.source }));
}

async function annotations(url: URL): Promise<RawAnnotation[]> {
  const text = await (await fetchOk(url)).text();
  if (url.pathname.endsWith(".jsonl")) return jsonLines(text);
  const file = JSON.parse(text) as { include?: string[]; annotation?: RawAnnotation[] };
  const included = await Promise.all(
    (file.include ?? []).map(async (name) => jsonLines(await (await fetchOk(new URL(name, url))).text())),
  );
  return [...included.flat(), ...(file.annotation ?? [])];
}

registerLoader({
  accepts: (url) => /\.(chronofx|annotations\.json|jsonl)$/.test(url.pathname),
  source: (url) => new FixtureSource(url),
  annotations,
});
