// The worker that holds the WASM decoder and every recording the page has opened. The page posts
// requests; each answer goes back with its arrays transferred, not copied.

import init, { headLength, Recording } from "../../../crates/wasm/pkg/chrono_wasm.js";
import { parse } from "smol-toml";
import { FileBytes, HttpBytes, type Bytes } from "./bytes.ts";
import { FIRST_READ, Opened, setCacheBytes, type WasmApi } from "./opened.ts";
import type { Request, Reply } from "./protocol.ts";

let api: Promise<WasmApi> | null = null;
const loadApi = () =>
  (api ??= init({ module_or_path: new URL("./chrono.wasm", import.meta.url) }).then(() => ({ headLength, Recording })));

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

/** The sidecar's TOML and the files it includes, relative to its URL. */
async function sidecar(url: string, signal: AbortSignal): Promise<{ hand: string; names: string[]; texts: string[] }> {
  const hand = await fetchText(url, signal);
  let include: unknown;
  try {
    include = (parse(hand) as { include?: unknown }).include ?? [];
  } catch (e) {
    throw new Error(`${decodeURIComponent(new URL(url).pathname)}: ${e instanceof Error ? e.message : e}`);
  }
  const names = Array.isArray(include) ? include.filter((n): n is string => typeof n === "string") : [];
  const texts = await Promise.all(names.map((n) => fetchText(new URL(n, url).href, signal)));
  return { hand, names, texts };
}

async function handle(m: Request, signal: AbortSignal): Promise<unknown> {
  switch (m.op) {
    case "open": {
      if (m.cacheBytes) setCacheBytes(m.cacheBytes);
      let r = opened.get(m.key);
      if (!r) {
        const bytes: Bytes = m.file ? new FileBytes(m.file, m.file.name) : new HttpBytes(m.url!);
        r = loadApi().then((a) => Opened.open(a, bytes, m.firstRead ?? FIRST_READ));
        opened.set(m.key, r);
        r.catch(() => opened.delete(m.key));
      }
      return (await r).info;
    }
    case "edges":
      return (await recording(m.key)).edges(m.channel, m.start, m.end, signal);
    case "samples":
      return (await recording(m.key)).samples(m.channel, m.start, m.end, signal);
    case "overview":
      return (await recording(m.key)).overview(m.channel, m.start, m.end, m.maxBucket, signal);
    case "sidecar": {
      const r = await recording(m.key);
      const { hand, names, texts } = await sidecar(m.url, signal);
      return r.resolveAnnotations(hand, names, texts, m.recordingFile);
    }
    case "resolve":
      return (await recording(m.key)).resolveAnnotations(m.hand, m.names, m.texts, m.recordingFile);
    case "bundled": {
      const r = await recording(m.key);
      const files = r.header.annotations;
      if (!files) return null;
      const hand = Object.keys(files).filter((n) => n.endsWith(".toml"));
      if (hand.length !== 1) throw new Error(`the bundled sidecar has ${hand.length} TOML files, not one`);
      const names = Object.keys(files).filter((n) => n !== hand[0]);
      // The sidecar travels inside the recording, so the file's own name proves nothing.
      return r.resolveAnnotations(files[hand[0]], names, names.map((n) => files[n]), null);
    }
  }
}

/** The buffers in an answer, to transfer rather than copy. */
function buffers(v: unknown): Transferable[] {
  if (!v || typeof v !== "object") return [];
  if (ArrayBuffer.isView(v)) return [v.buffer as ArrayBuffer];
  return Object.values(v).flatMap((x) => (ArrayBuffer.isView(x) ? [x.buffer as ArrayBuffer] : []));
}

self.onmessage = async (e: MessageEvent<Request | { op: "abort"; id: number }>) => {
  const m = e.data;
  if (m.op === "abort") {
    running.get(m.id)?.abort();
    return;
  }
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
};
