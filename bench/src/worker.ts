// The decode worker, as the viewer will run one. The page sends requests; the worker fetches or
// holds the bytes, decodes, and sends the arrays back by transfer.

import { canonical, canonicalOverview, sha256 } from "./canonical.ts";
import type { Block } from "./format.ts";
import { FIRST_FETCH } from "./format.ts";
import type { Decoder, Opened } from "./decoders.ts";

const started = performance.timeOrigin + performance.now();
const abs = () => performance.timeOrigin + performance.now();

async function range(url: string, from: number, to: number): Promise<Uint8Array> {
  const r = await fetch(url, { headers: { Range: `bytes=${from}-${to - 1}` } });
  if (r.status !== 206) throw new Error(`${url} ${from}-${to}: HTTP ${r.status}, not 206`);
  const b = new Uint8Array(await r.arrayBuffer());
  if (b.length !== to - from) throw new Error(`${url} ${from}-${to}: ${b.length} bytes`);
  return b;
}

async function fetchHead(dec: Decoder, url: string, ready: Promise<void>): Promise<Uint8Array> {
  const first = await range(url, 0, FIRST_FETCH);
  await ready;
  const need = dec.headLength(first);
  if (need <= first.length) return first.subarray(0, need);
  const rest = await range(url, first.length, need);
  const all = new Uint8Array(need);
  all.set(first);
  all.set(rest, first.length);
  return all;
}

function items(b: Block): { edges: number; samples: number } {
  let edges = 0;
  let samples = 0;
  for (const c of b.channels) {
    if (c.kind === "digital") edges += c.edges.length;
    else samples += c.values.length;
  }
  return { edges, samples };
}

function transferList(b: Block): ArrayBuffer[] {
  return b.channels.map((c) => (c.kind === "digital" ? c.edges.buffer : c.values.buffer) as ArrayBuffer);
}

export function serve(dec: Decoder): void {
  let file: Uint8Array | null = null;
  let opened: Opened | null = null;
  let inited: Promise<void> | null = null;
  const init = () => (inited ??= dec.init());
  const frameOf = (i: number) => {
    const [a, b] = opened!.blockRange(i);
    return file!.subarray(a, b);
  };

  const handle = async (m: any): Promise<[unknown, Transferable[]]> => {
    switch (m.op) {
      // Cold: decoder, head and one block, timed in absolute ms for the page to place.
      case "inset": {
        const t: Record<string, number> = { workerStarted: started, message: abs() };
        const ready = init().then(() => void (t.init = abs()));
        const head = await fetchHead(dec, m.url, ready);
        t.head = abs();
        const o = await dec.open(head, false);
        t.open = abs();
        const i = m.block ?? Math.floor(o.blockCount / 2);
        const [a, b] = o.blockRange(i);
        const frame = await range(m.url, a, b);
        t.block = abs();
        const block = await o.decode(i, frame);
        t.decoded = abs();
        return [{ t, block, i, headBytes: head.length, blockBytes: frame.length }, transferList(block)];
      }
      case "load": {
        await init();
        file = new Uint8Array(m.bytes);
        opened = await dec.open(file.subarray(0, dec.headLength(file.subarray(0, FIRST_FETCH))), true);
        return [{ blockCount: opened.blockCount }, []];
      }
      case "decode": {
        const s = performance.now();
        const block = await opened!.decode(m.i, frameOf(m.i));
        const ms = performance.now() - s;
        return [{ ms, phases: { ...opened!.phases }, ...items(block), block }, transferList(block)];
      }
      case "whole": {
        let edges = 0;
        let samples = 0;
        const s = performance.now();
        for (let i = 0; i < opened!.blockCount; i++) {
          const n = items(await opened!.decode(i, frameOf(i)));
          edges += n.edges;
          samples += n.samples;
        }
        return [{ ms: performance.now() - s, edges, samples }, []];
      }
      case "hash": {
        const hashes: string[] = [];
        // A block the decoder refuses is recorded, not thrown, so it counts as a mismatch.
        for (let i = 0; i < opened!.blockCount; i++) {
          try {
            hashes.push(await sha256(canonical(await opened!.decode(i, frameOf(i)))));
          } catch (e) {
            hashes.push(`refused: ${e}`);
          }
        }
        return [{ hashes }, []];
      }
      case "hashOverview": {
        const o = opened!;
        if (o.overviewCount === null) return [{ hashes: null }, []];
        const hashes: string[] = [];
        for (let j = 0; j < o.overviewCount; j++) {
          const [a, b] = o.overviewRange(j);
          try {
            hashes.push(await sha256(canonicalOverview(await o.decodeOverview(j, file!.subarray(a, b)))));
          } catch (e) {
            hashes.push(`refused: ${e}`);
          }
        }
        return [{ hashes }, []];
      }
      // One corrupted block: a frame to decode against `url`'s head, or a whole file.
      // Says where the decoder refused: opening the head, the control block, or the block itself.
      case "corrupt": {
        let o: Opened;
        // headLength parses the whole head in chrono-wasm, so a head refusal can come from either.
        try {
          o = await dec.open(await fetchHead(dec, m.url, init()), false);
        } catch (e) {
          return [{ refused: true, stage: "head", error: String(e) }, []];
        }
        const frame = m.frame ? new Uint8Array(m.frame) : await range(m.url, ...o.blockRange(m.block));
        // Control: a block the corruption does not touch decodes.
        const other = m.block === 0 ? o.blockCount - 1 : 0;
        try {
          await o.decode(other, await range(m.url, ...o.blockRange(other)));
        } catch (e) {
          return [{ refused: true, stage: "control", error: String(e) }, []];
        }
        try {
          await o.decode(m.block, frame);
          return [{ refused: false }, []];
        } catch (e) {
          return [{ refused: true, stage: "block", error: String(e) }, []];
        }
      }
    }
    throw new Error(`unknown op ${m.op}`);
  };

  self.onmessage = (ev: MessageEvent) => {
    handle(ev.data).then(
      ([reply, transfer]) => postMessage({ id: ev.data.id, ok: true, ...(reply as object) }, { transfer }),
      (e) => postMessage({ id: ev.data.id, ok: false, error: `${e}\n${e?.stack ?? ""}` }),
    );
  };
  postMessage({ id: "ready", ok: true, t: started });
}
