// A recording opened through the WASM decoder: its head parsed, its blocks fetched by range and
// decoded on demand, kept in an LRU. Runs in the worker; the unit tests run it in Node.

import { LRUCache } from "lru-cache";
import type { Recording as WasmRecording } from "../../../crates/wasm/pkg/tickwave_wasm.js";
import { lowerBound } from "../search.ts";
import {
  aborted,
  named,
  sampleTick,
  type AnalogOverview,
  type Channel,
  type DigitalOverview,
  type Edges,
  type Ratio,
  type RecordingInfo,
  type Samples,
} from "../source.ts";
import { join, type Bytes } from "./bytes.ts";
import { FIRST_READ } from "./protocol.ts";

/** What the opened recording needs from the wasm-bindgen module. */
export interface WasmApi {
  headLength(bytes: Uint8Array, maxDecoded?: number): number;
  Recording: new (head: Uint8Array, maxDecoded?: number) => WasmRecording;
}

interface DecodedBlock {
  readonly start: number;
  readonly end: number;
  readonly digital: ({ readonly initial: 0 | 1; readonly edges: Float64Array } | null)[];
  readonly analog: ({ readonly first: number; readonly values: Float32Array } | null)[];
  readonly size: number;
}

interface HeaderChannel {
  name: string;
  kind: "digital" | "analog";
  end?: number;
  unit?: string;
  first?: number;
  period?: [number, number];
  count?: number;
  coding?: { type: "f32" } | { type: "codes"; bits: number; scale: number; offset: number };
}

export interface Header {
  timebase: [number, number];
  length: number;
  channels: HeaderChannel[];
  title?: string;
  meta?: Record<string, string>;
  source?: Record<string, unknown>;
  content_id: string;
  blocks: number;
  overview: number[];
  /** Finer levels, kept in overview blocks after the data blocks, finest first. */
  overview_blocks?: { shift: number; blocks: number }[];
  annotations?: Record<string, string>;
}

/** One overview block's buckets, as the head's levels lay them out. */
interface DecodedOverview {
  readonly first: number;
  readonly buckets: number;
  /** Per channel: [level bits, change bits] for digital, [min, max] for analog. */
  readonly channels: (Uint8Array[] | Float32Array[])[];
  readonly size: number;
}

interface Sized {
  readonly size: number;
}

/** Decoded data and overview blocks of every recording the worker holds, by decoded size. */
let cache = new LRUCache<string, Sized>({ maxSize: 192 << 20, sizeCalculation: (b) => b.size });

/** Sizes the cache. Every recording's open asks for the page's size, so the same size changes
 * nothing; a new one keeps the blocks that fit, the most recently used first. */
export function setCacheBytes(bytes: number): void {
  if (bytes === cache.maxSize) return;
  const next = new LRUCache<string, Sized>({ maxSize: bytes, sizeCalculation: (b) => b.size });
  for (const key of cache.rkeys()) next.set(key, cache.peek(key)!);
  cache = next;
}

let nextId = 0;

/** A fetch of one or more adjacent blocks, shared by every request that waits on them. */
interface Flight<T> {
  readonly first: number;
  readonly last: number;
  readonly result: Promise<Map<number, T>>;
  readonly controller: AbortController;
  waiters: number;
}

/**
 * One kind of block in one recording, read by range and decoded on demand: the data blocks, or
 * the blocks of one finer overview level. Adjacent missing blocks share one read. An abort stops
 * waiting at once, and stops a read no other request waits on.
 */
class Shelf<T extends Sized> {
  readonly #key: string;
  readonly #bytes: Bytes;
  readonly #range: (i: number) => [number, number];
  readonly #decode: (i: number, frame: Uint8Array) => T;
  readonly #flights = new Map<number, Flight<T>>();

  constructor(key: string, bytes: Bytes, range: (i: number) => [number, number], decode: (i: number, frame: Uint8Array) => T) {
    this.#key = key;
    this.#bytes = bytes;
    this.#range = range;
    this.#decode = decode;
  }

  #fly(first: number, last: number): Flight<T> {
    const controller = new AbortController();
    const [from] = this.#range(first);
    const [, to] = this.#range(last);
    const result = (async () => {
      const bytes = await this.#bytes.read(from, to, controller.signal);
      if (bytes.length !== to - from) throw new Error(`${this.#bytes.name}: the file ends inside block ${last}`);
      const out = new Map<number, T>();
      for (let i = first; i <= last; i++) {
        const [a, b] = this.#range(i);
        const block = this.#decode(i, bytes.subarray(a - from, b - from));
        cache.set(`${this.#key} ${i}`, block);
        out.set(i, block);
      }
      return out;
    })();
    const flight: Flight<T> = { first, last, result, controller, waiters: 0 };
    for (let i = first; i <= last; i++) this.#flights.set(i, flight);
    result.then(
      () => this.#land(flight),
      () => this.#land(flight),
    );
    return flight;
  }

  /** Takes a flight off the board, so the next request for its blocks starts afresh. */
  #land(flight: Flight<T>): void {
    for (let i = flight.first; i <= flight.last; i++) if (this.#flights.get(i) === flight) this.#flights.delete(i);
  }

  /** Blocks [b0, b1), from the cache or fetched. */
  async get(b0: number, b1: number, signal?: AbortSignal): Promise<T[]> {
    if (signal?.aborted) throw aborted();
    const got = new Map<number, T>();
    const joined = new Set<Flight<T>>();
    let runStart = -1;
    const flush = (end: number) => {
      if (runStart >= 0) joined.add(this.#fly(runStart, end - 1));
      runStart = -1;
    };
    for (let i = b0; i < b1; i++) {
      const cached = cache.get(`${this.#key} ${i}`) as T | undefined;
      const flying = this.#flights.get(i);
      if (cached || flying) {
        flush(i);
        if (cached) got.set(i, cached);
        else joined.add(flying!);
      } else if (runStart < 0) runStart = i;
    }
    flush(b1);
    if (joined.size === 0) return [...got.keys()].sort((a, b) => a - b).map((i) => got.get(i)!);
    for (const f of joined) f.waiters++;
    // An aborted read must not be joined by a later request, so it lands as it aborts.
    const leave = () => {
      for (const f of joined) {
        if (--f.waiters > 0) continue;
        this.#land(f);
        f.controller.abort();
      }
    };
    try {
      const results = await new Promise<Map<number, T>[]>((resolve, reject) => {
        const onAbort = () => reject(aborted());
        signal?.addEventListener("abort", onAbort, { once: true });
        Promise.all([...joined].map((f) => f.result)).then(
          (r) => {
            signal?.removeEventListener("abort", onAbort);
            resolve(r);
          },
          (e) => {
            signal?.removeEventListener("abort", onAbort);
            reject(e);
          },
        );
      });
      for (const m of results) for (const [i, b] of m) if (i >= b0 && i < b1) got.set(i, b);
    } catch (e) {
      leave();
      throw e;
    }
    for (const f of joined) f.waiters--;
    return [...got.keys()].sort((a, b) => a - b).map((i) => got.get(i)!);
  }
}

export class Opened {
  readonly header: Header;
  readonly info: RecordingInfo;
  readonly #rec: WasmRecording;
  readonly #id = nextId++;
  readonly #data: Shelf<DecodedBlock>;
  readonly #levels: Shelf<DecodedOverview>[];
  readonly #overviews = new Map<string, Uint8Array[] | Float32Array[] | null>();

  private constructor(rec: WasmRecording, head: Uint8Array, bytes: Bytes) {
    this.#rec = rec;
    this.header = JSON.parse(rec.header) as Header;
    this.#data = new Shelf(`${this.#id} d`, bytes, (i) => this.#pair(rec.blockRange(i)), (i, f) => this.#decode(i, f));
    this.#levels = (this.header.overview_blocks ?? []).map(
      (_, level) =>
        new Shelf(
          `${this.#id} o${level}`,
          bytes,
          (k) => this.#pair(rec.overviewBlockRange(level, k)),
          (k, f) => this.#decodeOverview(level, k, f),
        ),
    );
    const view = new DataView(head.buffer, head.byteOffset, head.byteLength);
    const fileLength = Number(view.getBigUint64(24, true));
    const h = this.header;
    const channels: Channel[] = h.channels.map((c, i) => {
      if (c.kind === "digital") return { kind: "digital", name: c.name, end: c.end! };
      const [min, max] = this.#range(i);
      const period = { num: c.period![0], den: c.period![1] };
      return {
        kind: "analog",
        name: c.name,
        // Past the last sample's period: an analog channel can stop well before the capture does.
        end: Math.min(h.length, sampleTick(c.first!, period, c.count!)),
        unit: c.unit ?? "",
        first: c.first!,
        period,
        count: c.count!,
        resolution: c.coding?.type === "codes" ? c.coding.scale : null,
        min,
        max,
      };
    });
    this.info = {
      title: h.title ?? bytes.name,
      timebase: { num: h.timebase[0], den: h.timebase[1] },
      length: h.length,
      channels,
      bytes: fileLength,
      contentId: h.content_id,
      ...(h.source && { source: h.source }),
      ...(h.meta && { meta: h.meta }),
    };
  }

  /** Reads the head: the first read, then the rest if `headLength` asks for more. */
  static async open(api: WasmApi, bytes: Bytes, firstRead = FIRST_READ, maxDecoded?: number, signal?: AbortSignal): Promise<Opened> {
    let head = await bytes.read(0, firstRead, signal);
    const headLength = (b: Uint8Array) => {
      try {
        return api.headLength(b, maxDecoded);
      } catch (e) {
        throw named("TickwaveNotRecording", `${bytes.name}: ${e instanceof Error ? e.message : e}`);
      }
    };
    for (let need = headLength(head); need > head.length; need = headLength(head)) {
      const more = await bytes.read(head.length, need, signal);
      if (more.length === 0) throw new Error(`${bytes.name}: the file ends inside its head`);
      head = join(head, more);
    }
    const length = headLength(head);
    return new Opened(new api.Recording(head.subarray(0, length), maxDecoded), head, bytes);
  }

  /** An analog channel's smallest and largest value, from the coarsest overview level. */
  #range(channel: number): [number, number] {
    const levels = this.header.overview.length;
    const ov = levels > 0 ? (this.#overview(levels - 1, channel) as Float32Array[] | null) : null;
    let min = Infinity;
    let max = -Infinity;
    if (ov) {
      for (const v of ov[0]) if (v < min) min = v;
      for (const v of ov[1]) if (v > max) max = v;
    }
    return [min, max];
  }

  #overview(level: number, channel: number): Uint8Array[] | Float32Array[] | null {
    const key = `${level} ${channel}`;
    if (!this.#overviews.has(key)) {
      const kind = this.header.channels[channel].kind;
      const ov = kind === "digital" ? this.#rec.overviewDigital(level, channel) : this.#rec.overviewAnalog(level, channel);
      this.#overviews.set(key, ov ?? null);
    }
    return this.#overviews.get(key)!;
  }

  /** The blocks holding any tick of [start, end), clamped to the capture, at least one. */
  #covering(start: number, end: number): [number, number] {
    const last = Math.max(0, this.header.length - 1);
    const s = Math.min(Math.max(0, start), last);
    const e = Math.max(s + 1, Math.min(end, this.header.length));
    const [b0, b1] = this.#rec.blocksCovering(s, e);
    return [b0, Math.max(b1, b0 + 1)];
  }

  #decode(i: number, frame: Uint8Array): DecodedBlock {
    const b = this.#rec.decodeBlock(i, frame);
    try {
      let size = 64;
      const digital: DecodedBlock["digital"] = [];
      const analog: DecodedBlock["analog"] = [];
      this.header.channels.forEach((c, k) => {
        if (c.kind === "digital") {
          const edges = b.edges(k)!;
          size += edges.byteLength;
          digital.push({ initial: b.initial(k) ? 1 : 0, edges });
          analog.push(null);
        } else {
          const values = b.values(k)!;
          size += values.byteLength;
          digital.push(null);
          analog.push({ first: b.firstIndex(k)!, values });
        }
      });
      return { start: b.start, end: b.end, digital, analog, size };
    } finally {
      b.free();
    }
  }

  #pair(r: Float64Array): [number, number] {
    return [r[0], r[1]];
  }

  #decodeOverview(level: number, k: number, frame: Uint8Array): DecodedOverview {
    const b = this.#rec.decodeOverviewBlock(level, k, frame);
    try {
      let size = 64;
      const channels = this.header.channels.map((c, i) => {
        const arrays = (c.kind === "digital" ? b.digital(i) : b.analog(i))!;
        for (const a of arrays) size += a.byteLength;
        return arrays;
      });
      return { first: b.first, buckets: b.buckets, channels, size };
    } finally {
      b.free();
    }
  }

  /** Data blocks [b0, b1). */
  blocks(b0: number, b1: number, signal?: AbortSignal): Promise<DecodedBlock[]> {
    return this.#data.get(b0, b1, signal);
  }

  async edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges> {
    const [b0, b1] = this.#covering(start, end);
    const list = await this.blocks(b0, b1, signal);
    const parts: Float64Array[] = [];
    let level: 0 | 1 = 0;
    list.forEach((b, k) => {
      const d = b.digital[channel];
      if (!d) throw new RangeError(`channel ${channel} is not digital`);
      const lo = lowerBound(d.edges, start);
      if (k === 0) level = (d.initial ^ (lo & 1)) as 0 | 1;
      parts.push(d.edges.subarray(lo, lowerBound(d.edges, end, lo)));
    });
    const ticks = new Float64Array(parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of parts) {
      ticks.set(p, at);
      at += p.length;
    }
    return { level, ticks };
  }

  async samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    const c = this.header.channels[channel];
    if (c.kind !== "analog") throw new RangeError(`channel ${channel} is not analog`);
    const period: Ratio = { num: c.period![0], den: c.period![1] };
    const index = (tick: number) => sampleIndex(c.first!, period, c.count!, tick);
    const i0 = index(start);
    const i1 = Math.max(i0, index(end));
    const values = new Float32Array(i1 - i0);
    if (i1 > i0) {
      const [b0, b1] = this.#covering(sampleTick(c.first!, period, i0), sampleTick(c.first!, period, i1 - 1) + 1);
      for (const b of await this.blocks(b0, b1, signal)) {
        const a = b.analog[channel]!;
        const lo = Math.max(i0, a.first);
        const hi = Math.min(i1, a.first + a.values.length);
        if (hi > lo) values.set(a.values.subarray(lo - a.first, hi - a.first), lo - i0);
      }
    }
    return { first: i0, values };
  }

  /**
   * The coarsest level no coarser than `maxBucket`, or null: one of the head's, or a finer one
   * read from its overview blocks.
   */
  async overview(channel: number, start: number, end: number, maxBucket: number, signal?: AbortSignal): Promise<DigitalOverview | AnalogOverview | null> {
    let shift = -1;
    let head = -1;
    let blocked = -1;
    this.header.overview.forEach((sh, i) => {
      if (2 ** sh <= maxBucket && sh > shift) [shift, head, blocked] = [sh, i, -1];
    });
    (this.header.overview_blocks ?? []).forEach((l, i) => {
      if (2 ** l.shift <= maxBucket && l.shift > shift) [shift, head, blocked] = [l.shift, -1, i];
    });
    if (shift < 0) return null;
    const bucket = 2 ** shift;
    const count = Math.floor(this.header.length / bucket) + 1;
    const c = this.header.channels[channel];
    const k0 = Math.min(count, Math.max(0, Math.floor(start / bucket)));
    const k1 = Math.min(count, Math.max(k0, Math.ceil(Math.min(end, this.info.channels[channel].end) / bucket)));
    // Each source of buckets: [first bucket, arrays], arrays laid out as the head's.
    let parts: [number, Uint8Array[] | Float32Array[]][];
    if (head >= 0) {
      const ov = this.#overview(head, channel);
      if (!ov) return null;
      parts = [[0, ov]];
    } else {
      const [b0, b1] = this.#rec.overviewBlocksCovering(blocked, k0 * bucket, k1 * bucket);
      const blocks = await this.#levels[blocked].get(b0, b1, signal);
      parts = blocks.map((b) => [b.first, b.channels[channel]]);
    }
    const n = k1 - k0;
    if (c.kind === "digital") {
      const level = new Uint8Array(n);
      const changes = new Uint8Array(n);
      for (const [first, arrays] of parts) {
        const [bits, changeBits] = arrays as Uint8Array[];
        const from = Math.max(k0, first);
        const to = Math.min(k1, first + bits.length * 8);
        for (let k = from; k < to; k++) {
          const i = k - first;
          level[k - k0] = (bits[i >> 3] >> (i & 7)) & 1;
          changes[k - k0] = (changeBits[i >> 3] >> (i & 7)) & 1;
        }
      }
      return { kind: "digital", bucket, start: k0 * bucket, level, changes };
    }
    const min = new Float32Array(n).fill(Infinity);
    const max = new Float32Array(n).fill(-Infinity);
    for (const [first, arrays] of parts) {
      const [lo, hi] = arrays as Float32Array[];
      const from = Math.max(k0, first);
      const to = Math.min(k1, first + lo.length);
      if (to > from) {
        min.set(lo.subarray(from - first, to - first), from - k0);
        max.set(hi.subarray(from - first, to - first), from - k0);
      }
    }
    return { kind: "analog", bucket, start: k0 * bucket, min, max };
  }

  blockStart(i: number): number {
    return this.#rec.blockStart(i);
  }

  /** Resolves a sidecar (the hand-written file as JSON, and the files it includes) against this recording. */
  resolveAnnotations(hand: string, names: string[], texts: string[], recordingFile: string | null): string {
    return this.#rec.resolveAnnotations(hand, names, texts, recordingFile);
  }
}

/** The first sample at or after `tick`, clamped to [0, count]. */
function sampleIndex(first: number, period: Ratio, count: number, tick: number): number {
  let i = Math.ceil(((tick - first) * period.den) / period.num);
  i = Math.min(Math.max(i, 0), count);
  while (i > 0 && sampleTick(first, period, i - 1) >= tick) i--;
  while (i < count && sampleTick(first, period, i) < tick) i++;
  return i;
}
