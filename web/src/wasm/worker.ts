// The worker that holds the WASM decoder and every recording the page has opened. The page posts
// requests; each answer goes back with its arrays transferred, not copied.

import init, { footerStart, Recording } from "../../../crates/wasm/pkg/escapement_tickwave_wasm.js";
import { FileBytes, HttpBytes, type Bytes } from "./bytes.ts";
import { handJson } from "./hand.ts";
import { Opened, setCacheBytes, type WasmApi } from "./opened.ts";
import { FIRST_READ, type Request, type Reply } from "./protocol.ts";

let api: Promise<WasmApi> | null = null;
const loadApi = () =>
  (api ??= init({ module_or_path: new URL("./tickwave.wasm", import.meta.url) }).then(() => ({ footerStart, Recording })));

const opened = new Map<string, Promise<Opened>>();
const running = new Map<number, AbortController>();

async function recording(key: string): Promise<Opened> {
  const r = opened.get(key);
  if (!r) throw new Error(`${key} is not open`);
  return r;
}

async function fetchText(url: string, signal: AbortSignal): Promise<string> {
  const r = await fetch(url, { signal });
  if (!r.ok) throw new Error(`${decodeURIComponent(new URL(url).pathname)}: HTTP ${r.status}`);
  return r.text();
}

/** The sidecar's hand-written file as JSON, and the files it includes, relative to its URL. */
async function sidecar(url: string, signal: AbortSignal): Promise<{ hand: string; names: string[]; texts: string[] }> {
  const { json, include } = handJson(await fetchText(url, signal), decodeURIComponent(new URL(url).pathname));
  const texts = await Promise.all(include.map((n) => fetchText(new URL(n, url).href, signal)));
  return { hand: json, names: include, texts };
}

async function handle(m: Request, signal: AbortSignal): Promise<unknown> {
  switch (m.op) {
    case "open": {
      if (m.cacheBytes) setCacheBytes(m.cacheBytes);
      let r = opened.get(m.key);
      if (!r) {
        const bytes: Bytes = m.file ? new FileBytes(m.file, m.file.name) : new HttpBytes(m.url!);
        r = loadApi().then((a) => Opened.open(a, bytes, m.firstRead ?? FIRST_READ, m.maxDecoded));
        opened.set(m.key, r);
        r.catch(() => opened.delete(m.key));
      }
      return (await r).info;
    }
    case "edges":
      return (await recording(m.key)).edges(m.channel, m.start, m.end, signal);
    case "samples":
      return (await recording(m.key)).samples(m.channel, m.start, m.end, signal);
    case "edgeCount":
      return (await recording(m.key)).edgeCount(m.channel, m.start, m.end, signal);
    case "periods":
      return (await recording(m.key)).periods(m.channel, m.start, m.end, signal);
    case "overview":
      return (await recording(m.key)).overview(m.channel, m.start, m.end, m.maxBucket, signal);
    case "sidecar": {
      const r = await recording(m.key);
      const { hand, names, texts } = await sidecar(m.url, signal);
      return { json: r.resolveAnnotations(hand, names, texts, m.recordingFile), names };
    }
    case "resolve":
      return (await recording(m.key)).resolveAnnotations(handJson(m.hand, "the TOML file").json, m.names, m.texts, m.recordingFile);
    case "sidecarText": {
      const r = await recording(m.key);
      const { json, include } = handJson(m.hand, "the TOML file");
      const texts = m.base ? await Promise.all(include.map((n) => fetchText(new URL(n, m.base!).href, signal))) : [];
      return r.resolveAnnotations(json, m.base ? include : [], texts, m.recordingFile);
    }
    case "includes":
      if (m.url) return handJson(await fetchText(m.url, signal), decodeURIComponent(new URL(m.url).pathname)).include;
      return handJson(m.hand ?? "", m.where).include;
    case "bundled":
      // Read the first time a view asks: a first draw reads nothing past the footer.
      return (await recording(m.key)).annotations(m.span, signal);
  }
}

/** The buffers in an answer, to transfer rather than copy: its arrays, and those of a record in it, such as a sample tile's runs. */
function buffers(v: unknown): Transferable[] {
  if (!v || typeof v !== "object") return [];
  if (ArrayBuffer.isView(v)) return [v.buffer as ArrayBuffer];
  const views = (o: object) => Object.values(o).filter((x) => ArrayBuffer.isView(x));
  const nested = Object.values(v).flatMap((x) => (x && typeof x === "object" && !ArrayBuffer.isView(x) && !Array.isArray(x) ? views(x) : []));
  return [...new Set([...views(v), ...nested].map((x) => x.buffer as ArrayBuffer))];
}

self.onmessage = (e: MessageEvent<Request | { op: "abort"; id: number } | { op: "batch"; requests: Request[] }>) => {
  const m = e.data;
  if (m.op === "abort") running.get(m.id)?.abort();
  else if (m.op === "batch") for (const r of m.requests) void run(r);
  else void run(m);
};

async function run(m: Request): Promise<void> {
  const controller = new AbortController();
  running.set(m.id, controller);
  let reply: Reply;
  try {
    const value = await handle(m, controller.signal);
    reply = { id: m.id, ok: true, value };
  } catch (err) {
    const e = err as Error;
    reply = { id: m.id, ok: false, name: e?.name ?? "Error", message: e?.message ?? String(err) };
  } finally {
    running.delete(m.id);
  }
  (self as unknown as Worker).postMessage(reply, reply.ok ? buffers(reply.value) : []);
}
