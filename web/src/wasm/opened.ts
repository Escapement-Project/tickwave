// A recording opened through the WASM decoder: its footer parsed, its blocks fetched by range and
// decoded on demand, kept in an LRU. Runs in the worker; the unit tests run it in Node.

import { LRUCache } from "lru-cache";
import type { Recording as WasmRecording } from "../../../crates/wasm/pkg/escapement_tickwave_wasm.js";
import { lowerBound } from "../search.ts";
import {
  aborted,
  channelIndexAtOrAfter,
  channelSampleTick,
  fillRuns,
  named,
  placeAfter,
  SAMPLES_PER_RUN,
  type AnalogOverview,
  type Break,
  type Channel,
  type ContentId,
  type DigitalOverview,
  type Edges,
  type Gap,
  type RecordingInfo,
  type Samples,
  type SourceHeader,
} from "../source.ts";
import { join, type Bytes } from "./bytes.ts";
import { FIRST_READ } from "./protocol.ts";

/** What the opened recording needs from the wasm-bindgen module. */
export interface WasmApi {
  footerStart(tail: Uint8Array, fileLen: number, maxDecoded?: number): number;
  Recording: new (tail: Uint8Array, fileLen: number, maxDecoded?: number) => WasmRecording;
}

interface DecodedBlock {
  readonly start: number;
  readonly end: number;
  readonly digital: ({ readonly initial: 0 | 1; readonly edges: Float64Array } | null)[];
  readonly analog: ({ readonly first: number; readonly values: Float32Array | Float64Array } | null)[];
  readonly size: number;
}

interface HeaderShaped {
  offset?: number;
  offset_kind?: string;
  kept?: [number, number];
}

interface HeaderDigital extends HeaderShaped {
  name: string;
  source_name: string;
  source: number;
  kind: "digital";
  initial: boolean;
  end: number;
  edges: number;
  gaps?: [number, number, boolean][];
}

/** How an analog channel's samples are stored (FORMAT.md, "Codings"). */
type Coding =
  | { type: "f32" }
  | { type: "f64" }
  | { type: "int"; bits: number; signed: boolean; scale?: [number, number]; offset?: [number, number] }
  | { type: "codes"; bits: number; scale: number; offset: number };

interface HeaderAnalog extends HeaderShaped {
  name: string;
  source_name: string;
  source: number;
  kind: "analog";
  unit: string;
  first: number;
  period: [number, number];
  count: number;
  /** [index, tick, samples lost before it]. */
  breaks?: [number, number, number][];
  coding: Coding;
  significant?: number;
}

/** A channel computed from two analog channels (FORMAT.md, "Derived channels"). */
interface HeaderDerived {
  name: string;
  unit: string;
  product: [string, string];
  significant?: number;
}

/** Whether a channel's values come through the decoder as f64: an f64 or integer coding. */
function wide(c: HeaderAnalog): boolean {
  return c.coding.type === "f64" || c.coding.type === "int";
}

/** The value one code step stands for, or null for a float channel. */
function resolution(coding: Coding): number | null {
  if (coding.type === "codes") return coding.scale;
  if (coding.type === "int") return coding.scale ? coding.scale[0] / coding.scale[1] : 1;
  return null;
}

/** A channel as the header states it (FORMAT.md, "Header"). */
type HeaderChannel = HeaderDigital | HeaderAnalog;

export interface Header {
  timebase: [number, number];
  length: number;
  channels: HeaderChannel[];
  derived?: HeaderDerived[];
  title?: string;
  meta?: Record<string, string>;
  sources: SourceHeader[];
  content_id: ContentId;
  blocks: number;
  overview: number[];
  /** Finer levels, kept in overview blocks after the data blocks, finest first. */
  overview_blocks?: { shift: number; blocks: number }[];
  annotations?: Record<string, string>;
}

/** One overview block's buckets, as the footer's levels lay them out. */
interface DecodedOverview {
  readonly first: number;
  readonly buckets: number;
  /** Per channel: [level bits, change bits] for digital, [min, max] for analog. */
  readonly channels: (Uint8Array[] | Float32Array[] | Float64Array[])[];
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
  readonly #overviews = new Map<string, Uint8Array[] | Float32Array[] | Float64Array[] | null>();

  private constructor(rec: WasmRecording, fileLength: number, bytes: Bytes) {
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
    const h = this.header;
    // A gap from tick 0 says where the channel's data starts; the others are gaps in it.
    const leading = (c: (typeof h.channels)[number]) => c.kind === "digital" && c.gaps?.[0]?.[0] === 0;
    /** What a channel moved or trimmed states, for the details a page lists. */
    const shaped = (c: HeaderShaped) => ({
      ...(c.offset !== undefined && { offset: { ticks: c.offset, kind: c.offset_kind ?? "" } }),
      ...(c.kept && { kept: { from: c.kept[0], to: c.kept[1] } }),
    });
    const channels: Channel[] = h.channels.map((c, i) => {
      if (c.kind === "digital") {
        const gaps: Gap[] = (c.gaps ?? []).slice(leading(c) ? 1 : 0).map(([from, to, level]) => ({ from, to, level: level ? 1 : 0 }));
        return {
          ...shaped(c),
          kind: "digital",
          name: c.name,
          sourceName: c.source_name,
          source: c.source,
          end: c.end,
          ...(leading(c) && { start: c.gaps![0][1] }),
          ...(gaps.length > 0 && { gaps }),
        };
      }
      const [min, max] = this.#range(i);
      const period = { num: c.period[0], den: c.period[1] };
      const breaks: Break[] = (c.breaks ?? []).map(([index, tick, lost]) => ({ index, tick, lost }));
      const runs = { first: c.first, period, breaks };
      return {
        ...shaped(c),
        kind: "analog",
        name: c.name,
        sourceName: c.source_name,
        source: c.source,
        // Where the last sample's time ends: an analog channel can stop well before the capture does.
        end: Math.min(h.length, c.count === 0 ? c.first : placeAfter(runs, c.count - 1)),
        ...(breaks.length > 0 && { breaks }),
        unit: c.unit,
        first: c.first,
        period,
        count: c.count,
        resolution: resolution(c.coding),
        ...(c.significant !== undefined && { significant: c.significant }),
        min,
        max,
      };
    });
    // A derived channel reads as an analog one after the stored channels, placed as its first
    // operand's samples are (they share first, period, count and breaks).
    for (const [d, dc] of (h.derived ?? []).entries()) {
      const [a, b] = dc.product.map((name) => channels.findIndex((c) => c.name === name));
      const ca = channels[a];
      const cb = channels[b];
      if (ca?.kind !== "analog" || cb?.kind !== "analog") throw named("TickwaveNotRecording", `${bytes.name}: ${dc.name} is the product of channels it doesn't hold`);
      const [min, max] = this.#range(h.channels.length + d);
      const [sa, sb] = [a, b].map((i) => (h.channels[i] as HeaderAnalog).coding);
      // An integer product's step is the product of the two exact scales, divided once.
      const step = sa.type === "int" && sb.type === "int" ? ((sa.scale?.[0] ?? 1) * (sb.scale?.[0] ?? 1)) / ((sa.scale?.[1] ?? 1) * (sb.scale?.[1] ?? 1)) : null;
      channels.push({
        ...ca,
        name: dc.name,
        sourceName: dc.name,
        unit: dc.unit,
        product: [dc.product[0], dc.product[1]],
        resolution: step,
        ...(dc.significant !== undefined ? { significant: dc.significant } : { significant: undefined }),
        min,
        max,
      });
    }
    this.info = {
      title: h.title ?? bytes.name,
      timebase: { num: h.timebase[0], den: h.timebase[1] },
      length: h.length,
      channels,
      bytes: fileLength,
      contentId: h.content_id,
      sources: h.sources,
      ...(h.meta && { meta: h.meta }),
    };
  }

  /** Reads the footer: the file's last bytes, then the rest of it if `footerStart` asks. */
  static async open(api: WasmApi, bytes: Bytes, firstRead = FIRST_READ, maxDecoded?: number, signal?: AbortSignal): Promise<Opened> {
    let { bytes: tail, size } = await bytes.tail(firstRead, signal);
    const footerStart = (t: Uint8Array) => {
      try {
        return api.footerStart(t, size, maxDecoded);
      } catch (e) {
        throw named("TickwaveNotRecording", `${bytes.name}: ${e instanceof Error ? e.message : e}`);
      }
    };
    for (let from = size - tail.length, need = footerStart(tail); need < from; from = size - tail.length, need = footerStart(tail)) {
      const more = await bytes.read(need, from, signal);
      if (more.length !== from - need) throw new Error(`${bytes.name}: the file changed while it was read`);
      tail = join(more, tail);
    }
    return new Opened(new api.Recording(tail, size, maxDecoded), size, bytes);
  }

  /** An analog channel's smallest and largest value, from the coarsest overview level. */
  #range(channel: number): [number, number] {
    const levels = this.header.overview.length;
    const ov = levels > 0 ? (this.#overview(levels - 1, channel) as Float32Array[] | Float64Array[] | null) : null;
    let min = Infinity;
    let max = -Infinity;
    if (ov) {
      for (const v of ov[0]) if (v < min) min = v;
      for (const v of ov[1]) if (v > max) max = v;
    }
    return [min, max];
  }

  #overview(level: number, channel: number): Uint8Array[] | Float32Array[] | Float64Array[] | null {
    const key = `${level} ${channel}`;
    if (!this.#overviews.has(key)) {
      const c = this.header.channels[channel];
      const C = this.header.channels.length;
      const ov = !c
        ? this.#rec.overviewDerived(level, channel - C)?.slice(0, 2)
        : c.kind === "digital"
          ? this.#rec.overviewDigital(level, channel)
          : wide(c)
            ? this.#rec.overviewAnalog64(level, channel)
            : this.#rec.overviewAnalog(level, channel);
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
          const values = wide(c) ? b.values64(k)! : b.values(k)!;
          size += values.byteLength;
          digital.push(null);
          analog.push({ first: b.firstIndex(k)!, values });
        }
      });
      (this.header.derived ?? []).forEach((_, d) => {
        const values = b.derived(d)!;
        size += values.byteLength;
        digital.push(null);
        analog.push({ first: b.firstIndex(this.header.channels.findIndex((c) => c.name === this.header.derived![d].product[0]))!, values });
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
        const arrays = (c.kind === "digital" ? b.digital(i) : wide(c) ? b.analog64(i) : b.analog(i))!;
        for (const a of arrays) size += a.byteLength;
        return arrays;
      });
      (this.header.derived ?? []).forEach((_, d) => {
        const arrays = b.derived(d)!.slice(0, 2);
        for (const a of arrays) size += a.byteLength;
        channels.push(arrays);
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
    const c = this.header.channels[channel];
    // No edge lies in a gap, and a block's level bit carries the level from before one: the last
    // gap ending in the first block, at or before `start`, sets the level the edges after it flip.
    const gaps = c.kind === "digital" ? (c.gaps ?? []) : [];
    const parts: Float64Array[] = [];
    let level: 0 | 1 = 0;
    list.forEach((b, k) => {
      const d = b.digital[channel];
      if (!d) throw new RangeError(`channel ${channel} is not digital`);
      const lo = lowerBound(d.edges, start);
      if (k === 0) {
        level = (d.initial ^ (lo & 1)) as 0 | 1;
        let gap: [number, number, boolean] | null = null;
        for (const g of gaps) if (b.start < g[1] && g[1] <= start) gap = g;
        if (gap) level = ((gap[2] ? 1 : 0) ^ ((lo - lowerBound(d.edges, gap[1])) & 1)) as 0 | 1;
      }
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
    const c = this.header.channels[channel] as HeaderChannel | undefined;
    const info = this.info.channels[channel];
    if ((c && c.kind !== "analog") || info.kind !== "analog") throw new RangeError(`channel ${channel} is not analog`);
    const i0 = channelIndexAtOrAfter(info, start);
    const i1 = Math.max(i0, channelIndexAtOrAfter(info, end));
    // A derived channel's values are f64.
    const Values = !c || wide(c) ? Float64Array : Float32Array;
    const values = new Values(i1 - i0);
    if (i1 > i0) {
      const [b0, b1] = this.#covering(channelSampleTick(info, i0), channelSampleTick(info, i1 - 1) + 1);
      for (const b of await this.blocks(b0, b1, signal)) {
        const a = b.analog[channel]!;
        const lo = Math.max(i0, a.first);
        const hi = Math.min(i1, a.first + a.values.length);
        if (hi > lo) values.set(a.values.subarray(lo - a.first, hi - a.first), lo - i0);
      }
    }
    // Built here, off the page's thread, so a draw never builds them.
    const n = Math.floor(values.length / SAMPLES_PER_RUN);
    const runs = { min: new Values(n), max: new Values(n) };
    fillRuns(values, runs.min, runs.max, 0, n);
    return { first: i0, values, runs };
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
    const count = Math.ceil(this.header.length / bucket);
    const c = this.header.channels[channel] as HeaderChannel | undefined;
    const k0 = Math.min(count, Math.max(0, Math.floor(start / bucket)));
    const k1 = Math.min(count, Math.max(k0, Math.ceil(Math.min(end, this.info.channels[channel].end) / bucket)));
    // Each source of buckets: [first bucket, arrays], arrays laid out as the head's.
    let parts: [number, Uint8Array[] | Float32Array[] | Float64Array[]][];
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
    if (c?.kind === "digital") {
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
    const Values = !c || wide(c) ? Float64Array : Float32Array;
    const min = new Values(n).fill(Infinity);
    const max = new Values(n).fill(-Infinity);
    for (const [first, arrays] of parts) {
      const [lo, hi] = arrays as Float32Array[] | Float64Array[];
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
