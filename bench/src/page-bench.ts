// The block-decode page, driven by scripts/run.ts through `window.bench`. Each decoder runs in its
// own worker, all alive at once, so runs can alternate between decoders.

import { hasStreamZstd } from "./zstd.ts";

type Reply = { ok: boolean; error?: string; [k: string]: any };

class Client {
  private next = 0;
  private waiting = new Map<number, (r: Reply) => void>();
  readonly ready: Promise<void>;
  readonly w: Worker;
  constructor(dec: string) {
    this.w = new Worker(`dec/${dec}/worker.js`, { type: "module" });
    let up: () => void;
    this.ready = new Promise((r) => (up = r));
    this.w.onmessage = (ev) => {
      if (ev.data.id === "ready") return up();
      const f = this.waiting.get(ev.data.id);
      this.waiting.delete(ev.data.id);
      f?.(ev.data);
    };
  }
  call(msg: object, transfer: Transferable[] = []): Promise<Reply> {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, (r) => (r.ok ? resolve(r) : reject(new Error(r.error))));
      this.w.postMessage({ ...msg, id }, transfer);
    });
  }
}

const clients = new Map<string, Client>();
let blockCount = 0;

const bench = {
  features() {
    return { stream: hasStreamZstd(), crossOriginIsolated: self.crossOriginIsolated, userAgent: navigator.userAgent };
  },
  async setup(decoders: Record<string, string>) {
    for (const c of clients.values()) c.w.terminate();
    clients.clear();
    for (const [label, dec] of Object.entries(decoders)) clients.set(label, new Client(dec));
    await Promise.all([...clients.values()].map((c) => c.ready));
  },
  /** Fetches the file once and gives every worker its own copy. */
  async load(url: string) {
    const bytes = await (await fetch(url)).arrayBuffer();
    const r = await Promise.all([...clients.values()].map((c) => c.call({ op: "load", bytes })));
    blockCount = r[0].blockCount;
    return blockCount;
  },
  /** Every block once, through the worker: the round trip as the page sees it, and the worker's own time. */
  async pass(label: string) {
    const c = clients.get(label)!;
    const rt = new Float64Array(blockCount);
    const ms = new Float64Array(blockCount);
    const edges = new Float64Array(blockCount);
    const samples = new Float64Array(blockCount);
    const phases: Record<string, number[]> = {};
    for (let i = 0; i < blockCount; i++) {
      const s = performance.now();
      const r = await c.call({ op: "decode", i });
      rt[i] = performance.now() - s;
      ms[i] = r.ms;
      edges[i] = r.edges;
      samples[i] = r.samples;
      for (const [k, v] of Object.entries(r.phases as Record<string, number>)) (phases[k] ??= []).push(v);
    }
    return { rt: [...rt], ms: [...ms], edges: [...edges], samples: [...samples], phases };
  },
  async whole(label: string) {
    const r = await clients.get(label)!.call({ op: "whole" });
    return { ms: r.ms, edges: r.edges, samples: r.samples };
  },
  async hash(label: string) {
    return (await clients.get(label)!.call({ op: "hash" })).hashes as string[];
  },
  /** null when the decoder cannot decode overview blocks. */
  async hashOverview(label: string) {
    return (await clients.get(label)!.call({ op: "hashOverview" })).hashes as string[] | null;
  },
  async corrupt(label: string, url: string, block: number, frameUrl: string | null) {
    const frame = frameUrl ? await (await fetch(frameUrl)).arrayBuffer() : null;
    const r = await clients.get(label)!.call({ op: "corrupt", url, block, frame });
    return { refused: r.refused, stage: r.stage ?? null, error: r.error ?? null };
  },
};

(globalThis as any).bench = bench;
