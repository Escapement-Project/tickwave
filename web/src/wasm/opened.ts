// A recording opened through the WASM decoder: its footer parsed once, then for each channel a
// view asks for, the pages of its index and its list pages for the span asked, and the blocks and
// overview blocks they place, fetched by range and decoded on demand, kept in an LRU. Runs in the
// worker; the unit tests run it in Node.

import { LRUCache } from "lru-cache";
import type { Block as WasmBlock, OverviewLevel as WasmLevel, Recording as WasmRecording } from "../../../crates/wasm/pkg/escapement_tickwave_wasm.js";
import { DISTINCT_AT_MOST, PIECE_EDGES, PeriodCounter, type PeriodStats } from "../readouts.ts";
import { lowerBound } from "../search.ts";
import {
  aborted,
  axisTick,
  channelIndexAtOrAfter,
  channelSampleTick,
  fillRuns,
  firstAt,
  IDENTITY,
  named,
  placeAfter,
  SAMPLES_PER_RUN,
  type AnalogChannel,
  type AnalogOverview,
  type Break,
  type Channel,
  type ContentId,
  type DigitalOverview,
  type Edges,
  type Gap,
  type Listed,
  type Mapping,
  mOf,
  type RecordingInfo,
  type Samples,
  type SourceHeader,
} from "../source.ts";
import type { CoreAnnotation } from "../annotations.ts";
import { join, type Bytes } from "./bytes.ts";
import { handJson } from "./hand.ts";
import { FIRST_READ } from "./protocol.ts";

/** What the opened recording needs from the wasm-bindgen module. */
export interface WasmApi {
  footerStart(tail: Uint8Array, fileLen: number): number;
  Recording: new (tail: Uint8Array, fileLen: number, maxDecoded?: number) => WasmRecording;
}

interface HeaderShaped {
  offset?: number;
  offset_kind?: string;
  kept?: [number, number];
}

/** What a channel's header entry says of its tables (FORMAT.md, "Header"). */
interface HeaderTables {
  overview_shift?: number;
  overview_blocks?: { shift: number; blocks: number; rows: number }[];
}

/** What makes a channel unavailable to a reader that doesn't know it (FORMAT.md, "Unavailable
 * channels"). */
interface HeaderNeeds {
  requires?: string[];
  codec?: string;
}

interface HeaderDigital extends HeaderShaped, HeaderTables, HeaderNeeds {
  name: string;
  source_name: string;
  source: number;
  kind: "digital";
  initial: boolean;
  end: number;
  edges: number;
  blocks: number;
  gaps?: [number, number, boolean][];
  /** How many gaps the list pages hold, where they are in pages. */
  gap_count?: number;
}

/** How an analog channel's samples are stored (FORMAT.md, "Codings"). */
type Coding =
  | { type: "f32" }
  | { type: "f64" }
  | { type: "int"; bits: number; signed: boolean; scale?: [number, number]; offset?: [number, number] }
  | { type: "codes"; bits: number; scale: number; offset: number };

interface HeaderAnalog extends HeaderShaped, HeaderTables, HeaderNeeds {
  name: string;
  source_name: string;
  source: number;
  kind: "analog";
  unit: string;
  first: number;
  period: [number, number];
  count: number;
  blocks: number;
  /** Where the samples sit within a tick of the source's grid (FORMAT.md, "Times"). */
  phase?: [number, number];
  /** [index, tick, samples lost before it], and the run's phase in the grid's `1/d` ticks where
   * it is not 0. */
  breaks?: ([number, number, number] | [number, number, number, number])[];
  /** How many breaks the list pages hold, where they are in pages. */
  break_count?: number;
  lost_before?: number;
  lost_after?: number;
  coding: Coding;
  significant?: number;
}

/** A channel computed from analog channels (FORMAT.md, "Derived channels"). */
interface HeaderDerived extends HeaderTables {
  name: string;
  unit?: string;
  op: string;
  operands: string[];
  params?: Record<string, string>;
  significant?: number;
  requires?: string[];
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
  /** The finest overview level's shift of a channel that states none (FORMAT.md, "Overview"). */
  overview_shift: number;
  /** How many annotation parts the recording holds, where it holds any. */
  annotation_parts?: number;
}

interface Sized {
  readonly size: number;
}

/** One channel's block, decoded. */
type DecodedBlock = Sized &
  (
    | { readonly kind: "digital"; readonly start: number; readonly end: number; readonly initial: 0 | 1; readonly edges: Float64Array }
    | {
        readonly kind: "analog";
        readonly start: number;
        readonly end: number;
        readonly first: number;
        readonly values: Float32Array | Float64Array;
        /** An integer operand's codes, for a derived channel's exact result. */
        readonly codes?: BigInt64Array;
      }
  );

/**
 * The level before digital block `b`'s edge `lo`, its first at or past `start`. No edge lies in a
 * gap, and a block's level bit carries the level from before one until its `to`: the last of
 * `gaps` ending in the block or at its start, at or before `start`, sets the level the edges
 * after it flip.
 */
function levelBefore(b: DecodedBlock & { kind: "digital" }, lo: number, start: number, gaps: readonly Gap[] | null): 0 | 1 {
  let gap: Gap | null = null;
  for (const g of gaps ?? []) if (b.start <= g.to && g.to <= start) gap = g;
  if (gap) return (gap.level ^ ((lo - lowerBound(b.edges, gap.to)) & 1)) as 0 | 1;
  return (b.initial ^ (lo & 1)) as 0 | 1;
}

/** A level's buckets, laid out as `OverviewLevel` gives them: [density, duty] for a digital
 * channel, a byte a bucket each, and [min, max] for an analog one. */
type Arrays = Uint8Array[] | Float32Array[] | Float64Array[];

/** One overview block, decoded: the buckets of each of the `n` blocks it stands for. */
interface DecodedOverview extends Sized {
  readonly arrays: Arrays;
}

/** A page of a table, checked: its entries, a fixed count of numbers each. */
interface DecodedPage extends Sized {
  readonly entries: Float64Array;
}

/** Numbers a located page, a block, an overview row and a part row take (crates/wasm). */
const LOCATED = 7;
const BLOCK = 8;
const ROW = 5;
const PART = 6;

/** A line of an annotation part, as `partAnnotations` gives it: its id where it reads, and its
 * annotation or why it is left out; `past` the length, it claims no id. */
interface PartLine {
  readonly id?: string | null;
  readonly annotation?: CoreAnnotation;
  readonly left?: string;
  readonly past?: boolean;
}

/** An annotation part, read, each of its lines resolved alone. */
interface PartRead extends Sized {
  readonly source: string;
  readonly lines: readonly PartLine[];
}

/** The annotation bundle as reading annotations a range at a time needs it. */
interface BundleRead {
  /** The hand-written file as JSON, and the full ids its refs name. */
  readonly hand: string;
  readonly refs: ReadonlySet<string>;
  /** The generator files, each its first line alone. */
  readonly names: string[];
  readonly texts: string[];
  /** Each generator's file's place in the sidecar's order, from 1, by the generator's name; null
   * where its first line breaks a rule, which leaves out every line of it. */
  readonly ranks: ReadonlyMap<string, number | null>;
}

/** A bundled sidecar's annotations as far as they are read (FORMAT.md, "Annotations"). */
export interface AnnotationsRead {
  /** In the sidecar's order: the hand-written file's, then each generator file's lines. */
  readonly list: CoreAnnotation[];
  /** What reading them left out, and why. */
  readonly problems: string[];
  /** Whether every part is read: until then a hand-written `ref` and the bundle's own problems
   * wait, since a ref resolves only once every part is read. */
  readonly complete: boolean;
}

/** The generator a bundled file's first line names, where it is a generator file's head. */
function generatorOf(text: string): string | null {
  try {
    const head: unknown = JSON.parse(text.split("\n", 1)[0]);
    if (head && typeof head === "object" && "format" in head && "source" in head && typeof head.source === "string") return head.source;
  } catch {
    // Not JSON: the hand-written resolver says what is wrong with the file.
  }
  return null;
}
/** Buckets in an overview block (FORMAT.md, "Overview"). */
const BLOCK_BUCKETS = 4096;

/** Decoded blocks, pages and overview blocks of every recording the worker holds, by size. */
let cache = new LRUCache<string, Sized>({ maxSize: 192 << 20, sizeCalculation: (b) => b.size });

/** Sizes the cache. Every recording's open asks for the page's size, so the same size changes
 * nothing; a new one keeps the entries that fit, the most recently used first. */
export function setCacheBytes(bytes: number): void {
  if (bytes === cache.maxSize) return;
  const next = new LRUCache<string, Sized>({ maxSize: bytes, sizeCalculation: (b) => b.size });
  for (const key of cache.rkeys()) next.set(key, cache.peek(key)!);
  cache = next;
}

let nextId = 0;

/** A range of the file a request wants, and what it makes of the bytes. */
interface Want<T> {
  readonly key: string;
  readonly from: number;
  readonly to: number;
  readonly make: (bytes: Uint8Array) => T;
}

/**
 * Wanted ranges this close together share one read. A channel's next block follows its last one
 * in the file after the records other channels wrote meanwhile: 41 bytes when none did.
 */
const JOIN = 512;

/**
 * Runs `f` in a task of its own, after the current task and its microtasks. Not a timer: a query
 * waits for one at each step of its reads, and a timer waits at least 1 ms in Node, and 4 ms in a
 * browser once timers nest.
 */
function afterTask(f: () => void): void {
  const { port1, port2 } = new MessageChannel();
  port1.onmessage = () => {
    // An open port keeps Node running.
    port1.close();
    f();
  };
  port2.postMessage(null);
}

/** A read of one or more ranges, shared by every request that waits on them. */
interface Flight {
  readonly keys: readonly string[];
  readonly result: Promise<Map<string, Sized>>;
  readonly controller: AbortController;
  waiters: number;
}

/**
 * Ranges of one recording read and made into what a request wants, each kept in the cache under its
 * key. The ranges every request asks for in one turn are read together, and those lying close share
 * one read: a view asks for each of its channels apart, and Chromium starts a request to a URL only
 * once the one before it has its answer. An abort stops waiting at once, and stops a read no other
 * request waits on.
 */
class Reader {
  readonly #bytes: Bytes;
  readonly #flights = new Map<string, Flight>();
  /** The ranges asked for and not yet read, by key, until the turn ends. */
  readonly #queued = new Map<string, Want<Sized>>();
  #turn: Promise<void> | null = null;

  constructor(bytes: Bytes) {
    this.#bytes = bytes;
  }

  /** Resolves once the ranges queued in this turn are in flight. */
  #flush(): Promise<void> {
    this.#turn ??= new Promise((resolve) =>
      afterTask(() => {
        this.#turn = null;
        const order = [...this.#queued.values()].sort((a, b) => a.from - b.from);
        this.#queued.clear();
        for (let i = 0; i < order.length; ) {
          let j = i + 1;
          let to = order[i].to;
          while (j < order.length && order[j].from - to <= JOIN) to = Math.max(to, order[j++].to);
          this.#fly(order.slice(i, j));
          i = j;
        }
        resolve();
      }),
    );
    return this.#turn;
  }

  #fly(wants: readonly Want<Sized>[]): Flight {
    const controller = new AbortController();
    const from = wants[0].from;
    const to = Math.max(...wants.map((w) => w.to));
    const result = (async () => {
      const bytes = await this.#bytes.read(from, to, controller.signal);
      if (bytes.length !== to - from) throw new Error(`${this.#bytes.name}: the file ends before byte ${to}`);
      const out = new Map<string, Sized>();
      for (const w of wants) {
        const made = w.make(bytes.subarray(w.from - from, w.to - from));
        cache.set(w.key, made);
        out.set(w.key, made);
      }
      return out;
    })();
    const flight: Flight = { keys: wants.map((w) => w.key), result, controller, waiters: 0 };
    for (const k of flight.keys) this.#flights.set(k, flight);
    result.then(
      () => this.#land(flight),
      () => this.#land(flight),
    );
    return flight;
  }

  /** Takes a flight off the board, so the next request for its ranges starts afresh. */
  #land(flight: Flight): void {
    for (const k of flight.keys) if (this.#flights.get(k) === flight) this.#flights.delete(k);
  }

  /** What each of `wants` makes, from the cache or read, in their order. */
  async get<T extends Sized>(wants: readonly Want<T>[], signal?: AbortSignal): Promise<T[]> {
    if (signal?.aborted) throw aborted();
    const got = new Map<string, T>();
    const joined = new Set<Flight>();
    let fresh = new Map<string, Want<T>>();
    const sort = (w: Want<T>) => {
      const cached = cache.get(w.key) as T | undefined;
      const flying = this.#flights.get(w.key);
      if (cached) got.set(w.key, cached);
      else if (flying) joined.add(flying);
      else return true;
      return false;
    };
    for (const w of wants) if (!got.has(w.key) && !fresh.has(w.key) && sort(w)) fresh.set(w.key, w);
    // A range another request's abort took off the board before this one joined it is asked for
    // again in the next turn.
    while (fresh.size > 0) {
      for (const [k, w] of fresh) if (!this.#queued.has(k)) this.#queued.set(k, w);
      await this.#flush();
      if (signal?.aborted) {
        for (const k of fresh.keys()) {
          const f = this.#flights.get(k);
          if (f && f.waiters === 0) {
            this.#land(f);
            f.controller.abort();
          }
        }
        throw aborted();
      }
      const again = new Map<string, Want<T>>();
      for (const [k, w] of fresh) if (sort(w)) again.set(k, w);
      fresh = again;
    }
    if (joined.size > 0) {
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
        const results = await new Promise<Map<string, Sized>[]>((resolve, reject) => {
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
        for (const m of results) for (const [k, v] of m) got.set(k, v as T);
      } catch (e) {
        leave();
        throw e;
      }
      for (const f of joined) f.waiters--;
    }
    return wants.map((w) => got.get(w.key)!);
  }
}

/** Numbers an entry of a list takes, as `listPage` gives them: a gap's `from`, `to`, level and 0,
 * or a break's index, tick, samples lost and phase. */
const ENTRY = 4;

/** A break from a list's entry at `i`. */
const breakAt = (e: Float64Array, i: number): Break => ({ index: e[i], tick: e[i + 1], lost: e[i + 2], ...(e[i + 3] !== 0 && { phase: e[i + 3] }) });

/** A channel's gaps or breaks in pages over a span, `ENTRY` numbers an entry, as `listPage` gives
 * them, from the list's start where `first`, to its end where `toEnd`. */
interface Slice {
  readonly entries: Float64Array;
  readonly first: boolean;
  readonly toEnd: boolean;
}

/** An entry's key in a slice: a gap's `from`, a break's `tick`. */
function sliceKey(s: Float64Array, digital: boolean, i: number): number {
  return s[ENTRY * i + (digital ? 0 : 1)];
}

/** The entries of `s` that source ticks `[lo, hi)` need, as `Listed` words it: from the one
 * before the last at or before `lo` through the first at or past `hi`. */
function restrict(s: Float64Array, digital: boolean, lo: number, hi: number): Float64Array {
  const n = s.length / ENTRY;
  let atOrBefore = 0;
  while (atOrBefore < n && sliceKey(s, digital, atOrBefore) <= lo) atOrBefore++;
  let before = atOrBefore;
  while (before < n && sliceKey(s, digital, before) < hi) before++;
  return s.subarray(ENTRY * Math.max(0, atOrBefore - 2), ENTRY * Math.min(n, before + 1));
}

/** Pairs `[a, b, c, d, ...]` as `[[a, b], [c, d], ...]`. */
function pairs(r: Float64Array | number[]): [number, number][] {
  const out: [number, number][] = [];
  for (let i = 0; i + 1 < r.length; i += 2) out.push([r[i], r[i + 1]]);
  return out;
}

export class Opened {
  readonly header: Header;
  readonly info: RecordingInfo;
  readonly #rec: WasmRecording;
  readonly #id = nextId++;
  readonly #reader: Reader;
  readonly #name: string;
  /** Each channel's and derived channel's mapping to the axis, and its start in source ticks. */
  readonly #maps: Mapping[];
  readonly #starts: number[];
  /** Each digital channel's gaps from the header, on the axis, the one from its start included. */
  readonly #gaps: (Gap[] | null)[];
  /** Each overview channel's levels' shifts, finest first, the strip's last. */
  readonly #shifts = new Map<number, number[]>();
  readonly #strips = new Map<number, Arrays>();
  /** The stored `int` channels some derived channel takes, as codes. */
  readonly #operands = new Set<number>();

  private constructor(rec: WasmRecording, fileLength: number, bytes: Bytes) {
    this.#rec = rec;
    this.#reader = new Reader(bytes);
    this.#name = bytes.name;
    this.header = JSON.parse(rec.header) as Header;
    const h = this.header;
    const timebase = { num: h.timebase[0], den: h.timebase[1] };
    // Each source's `m` once: many channels share a source.
    const ms = h.sources.map((s) => mOf(timebase, s));
    /** Where a channel's source ticks lie on the axis: its source's origin and `m`, its offset. */
    const mapping = (c: HeaderShaped & { source: number }): Mapping => ({
      origin: h.sources[c.source].origin,
      offset: c.offset ?? 0,
      m: ms[c.source],
    });
    this.#maps = h.channels.map(mapping);
    // A channel's start in its source's ticks (FORMAT.md, "Header").
    this.#starts = h.channels.map((c) => c.kept?.[0] ?? -h.sources[c.source].origin);
    this.#gaps = h.channels.map((c, i) =>
      c.kind === "digital" && c.gap_count === undefined
        ? (c.gaps ?? []).map(([from, to, level]) => ({ from: axisTick(this.#maps[i], from), to: axisTick(this.#maps[i], to), level: level ? 1 : 0 }))
        : null,
    );
    const plain = (map: Mapping) => map.origin === 0 && map.offset === 0 && map.m.num === map.m.den;
    /** What a channel moved or trimmed states, for the details a page lists. */
    const shaped = (c: HeaderShaped) => ({
      ...(c.offset !== undefined && { offset: { ticks: c.offset, kind: c.offset_kind ?? "" } }),
      ...(c.kept && { kept: { from: c.kept[0], to: c.kept[1] } }),
    });
    const channels: Channel[] = h.channels.map((c, i) => {
      const map = this.#maps[i];
      // What this version can't read keeps its place and name (FORMAT.md, "Unavailable channels").
      const why = rec.unavailable(i);
      if (why !== undefined) return { kind: "unavailable", name: c.name, sourceName: c.source_name, source: c.source, why, end: h.length };
      if (c.kind === "digital") {
        // A gap from the channel's start says where its level is first known, and the others
        // are gaps in it.
        const first = this.#starts[i];
        const paged = c.gap_count !== undefined;
        const leading = !paged && c.gaps?.[0]?.[0] === first;
        const gaps = paged ? [] : this.#gaps[i]!.slice(leading ? 1 : 0);
        const start = axisTick(map, leading ? c.gaps![0][1] : first);
        return {
          ...shaped(c),
          kind: "digital",
          name: c.name,
          sourceName: c.source_name,
          source: c.source,
          end: axisTick(map, c.end),
          ...(start !== 0 && { start }),
          ...(gaps.length > 0 && { gaps }),
          ...(paged && { paged: true }),
          ...(!plain(map) && { map }),
        };
      }
      const period = { num: c.period[0], den: c.period[1] };
      const phase = c.phase && { num: c.phase[0], den: c.phase[1] };
      const paged = c.break_count !== undefined;
      const breaks: Break[] = (c.breaks ?? []).map(([index, tick, lost, phase]) => ({ index, tick, lost, ...(phase && { phase }) }));
      const runs = { first: c.first, period, phase, count: c.count, breaks, map };
      return {
        ...shaped(c),
        kind: "analog",
        name: c.name,
        sourceName: c.source_name,
        source: c.source,
        // Where the last sample's time ends: an analog channel can stop well before the capture
        // does. One whose breaks are in pages runs to the length until an answer reaches its end.
        end: paged ? h.length : Math.min(h.length, c.count === 0 ? axisTick(map, c.first) : placeAfter(runs, c.count - 1)),
        ...(breaks.length > 0 && { breaks }),
        ...(paged && { paged: true }),
        ...(c.lost_before && { lostBefore: c.lost_before }),
        ...(c.lost_after && { lostAfter: c.lost_after }),
        ...(!plain(map) && { map }),
        unit: c.unit,
        first: c.first,
        period,
        ...(phase && { phase }),
        count: c.count,
        resolution: resolution(c.coding),
        ...(c.significant !== undefined && { significant: c.significant }),
      };
    });
    // A derived channel reads as an analog one after the stored channels, placed as its first
    // operand's samples are (they share first, period, count, breaks and losses).
    for (const [d, dc] of (h.derived ?? []).entries()) {
      const o = h.channels.length + d;
      const [a, b] = dc.operands.map((name) => channels.findIndex((c) => c.name === name));
      // The decoder checked that its operands name channels, which give its place.
      this.#maps.push(this.#maps[a] ?? IDENTITY);
      this.#starts.push(this.#starts[a] ?? 0);
      const why = rec.unavailable(o);
      if (why !== undefined) {
        channels.push({ kind: "unavailable", name: dc.name, sourceName: dc.name, why, end: h.length });
        continue;
      }
      const ca = channels[a];
      const cb = channels[b];
      if (ca?.kind !== "analog" || cb?.kind !== "analog") throw named("TickwaveNotRecording", `${bytes.name}: ${dc.name} is derived from channels it doesn't hold`);
      for (const i of [a, b]) if ((h.channels[i] as HeaderAnalog).coding.type === "int") this.#operands.add(i);
      channels.push({
        ...ca,
        name: dc.name,
        sourceName: dc.name,
        unit: dc.unit ?? "",
        derived: { op: dc.op, operands: [...dc.operands] },
        resolution: rec.derivedStep(d) ?? null,
        ...(dc.significant !== undefined ? { significant: dc.significant } : { significant: undefined }),
      });
    }
    this.info = {
      title: h.title ?? bytes.name,
      timebase,
      length: h.length,
      channels,
      bytes: fileLength,
      contentId: h.content_id,
      sources: h.sources,
      ...(h.meta && { meta: h.meta }),
      ...(rec.bundleRange().length > 0 && { bundled: true }),
    };
  }

  /** Reads the footer: the file's last bytes, then the rest of it if the trailer places it
   * earlier, and parses it once. */
  static async open(api: WasmApi, bytes: Bytes, firstRead = FIRST_READ, maxDecoded?: number, signal?: AbortSignal): Promise<Opened> {
    let { bytes: tail, size } = await bytes.tail(firstRead, signal);
    const notRecording = (e: unknown) => named("TickwaveNotRecording", `${bytes.name}: ${e instanceof Error ? e.message : e}`);
    const footerStart = (t: Uint8Array) => {
      try {
        return api.footerStart(t, size);
      } catch (e) {
        throw notRecording(e);
      }
    };
    for (let from = size - tail.length, need = footerStart(tail); need < from; from = size - tail.length, need = footerStart(tail)) {
      const more = await bytes.read(need, from, signal);
      if (more.length !== from - need) throw new Error(`${bytes.name}: the file changed while it was read`);
      tail = join(more, tail);
    }
    let rec: WasmRecording;
    try {
      rec = new api.Recording(tail, size, maxDecoded);
    } catch (e) {
      throw notRecording(e);
    }
    return new Opened(rec, size, bytes);
  }

  /** Overview channel `o`'s levels' shifts, finest first, its strip's last. */
  #levels(o: number): number[] {
    let s = this.#shifts.get(o);
    if (!s) this.#shifts.set(o, (s = [...this.#rec.shifts(o)]));
    return s;
  }

  /** Overview channel `o`'s strip, its coarsest level, decoded once: from the footer. */
  #strip(o: number): Arrays {
    let arrays = this.#strips.get(o);
    if (arrays) return arrays;
    const level = this.#rec.strip(o);
    try {
      arrays = this.#arrays(o, level);
    } finally {
      level.free();
    }
    this.#strips.set(o, arrays);
    return arrays;
  }

  /** A decoded level's buckets as `Arrays`, by the channel's kind and coding. */
  #arrays(o: number, level: WasmLevel): Arrays {
    const c = this.header.channels[o] as HeaderChannel | undefined;
    const arrays = !c ? level.derived()?.slice(0, 2) : c.kind === "digital" ? level.digital() : wide(c) ? level.analog64() : level.analog();
    if (!arrays) throw new Error(`${this.#name}: channel ${o}'s overview is not laid out as its kind`);
    return arrays;
  }

  /** Whether stored channel `c`'s gaps or breaks are in list pages. */
  #paged(c: number): boolean {
    const h = this.header.channels[c];
    return h.kind === "digital" ? h.gap_count !== undefined : h.break_count !== undefined;
  }

  /** The stored channel whose list overview channel `o` takes: itself, or a derived channel's
   * first operand. */
  #listedBy(o: number): number {
    const C = this.header.channels.length;
    if (o < C) return o;
    return this.header.channels.findIndex((c) => c.name === this.header.derived![o - C].operands[0]);
  }

  /** The pages of a table that hold keys `[k0, k1)`, located through its directories, which are
   * fetched as the look-up asks for them. */
  async #located(look: () => Float64Array | undefined, signal?: AbortSignal): Promise<Float64Array[]> {
    let asked = "";
    for (;;) {
      const pages = look();
      if (pages) {
        const out: Float64Array[] = [];
        for (let i = 0; i < pages.length; i += LOCATED) out.push(pages.subarray(i, i + LOCATED));
        return out;
      }
      const ranges = pairs(this.#rec.missing());
      // Each round hands in a directory one depth further down, or the look-up has stalled.
      if (ranges.length === 0 || JSON.stringify(ranges) === asked) throw new Error(`${this.#name}: a look-up stopped short of its pages`);
      asked = JSON.stringify(ranges);
      const dirs = await this.#reader.get(
        ranges.map(([from, to]) => ({ key: `${this.#id} d${from}`, from, to, make: (bytes: Uint8Array) => ({ bytes: bytes.slice(), size: 64 + bytes.length }) })),
        signal,
      );
      // The decoder keeps what it is handed, so the next look-up finds it there.
      dirs.forEach((d, i) => this.#rec.addDirectory(ranges[i][0], d.bytes));
    }
  }

  /** The located pages `at`, each checked by `parse` and kept under `key`. */
  #pages(at: Float64Array[], key: (j: number) => string, parse: (at: Float64Array, bytes: Uint8Array) => Float64Array, signal?: AbortSignal): Promise<DecodedPage[]> {
    return this.#reader.get(
      at.map((p) => ({
        key: key(p[0]),
        from: p[1],
        to: p[1] + p[2],
        make: (bytes: Uint8Array): DecodedPage => {
          const entries = parse(p, bytes);
          return { entries, size: 64 + entries.byteLength };
        },
      })),
      signal,
    );
  }

  /** Stored channel `c`'s blocks holding any tick of `[start, end)`, each as `blockPage` gives
   * it, in order. */
  async #blockRefs(c: number, start: number, end: number, signal?: AbortSignal): Promise<Float64Array[]> {
    const at = await this.#located(() => this.#rec.blockPages(c, start, end), signal);
    const pages = await this.#pages(at, (j) => `${this.#id} b${c}.${j}`, (p, b) => this.#rec.blockPage(c, p, b), signal);
    const out: Float64Array[] = [];
    for (const { entries } of pages) {
      for (let i = 0; i < entries.length; i += BLOCK) {
        if (entries[i + 1] > start && entries[i] < end) out.push(entries.subarray(i, i + BLOCK));
      }
    }
    return out;
  }

  /** Stored channel `c`'s gaps or breaks in pages that source ticks `[lo, hi)` need, as `Listed`
   * words it, each page read once into the cache. */
  async #slice(c: number, lo: number, hi: number, signal?: AbortSignal): Promise<Slice> {
    const digital = this.header.channels[c].kind === "digital";
    const read = (k0: number, k1: number) => this.#located(() => this.#rec.listPages(c, k0, k1), signal);
    const parse = (at: Float64Array[]) => this.#pages(at, (j) => `${this.#id} g${c}.${j}`, (p, b) => this.#rec.listPage(c, p, b), signal);
    const at = await read(lo, hi);
    const pages = await parse(at);
    const count = (p: DecodedPage, f: (k: number) => boolean) => {
      let n = 0;
      for (let i = 0; i < p.entries.length / ENTRY; i++) if (f(sliceKey(p.entries, digital, i))) n++;
      return n;
    };
    // Two entries at or before the span's start: the page before the first where it holds fewer.
    if (at[0][0] > 0 && count(pages[0], (k) => k <= lo) < 2) {
      const before = await read(at[0][3] - 1, at[0][3]);
      if (before.length > 0 && before[0][0] + 1 === at[0][0]) {
        at.unshift(before[0]);
        pages.unshift((await parse([before[0]]))[0]);
      }
    }
    // The first entry at or past the span's end: the page after the last where it holds none.
    const last = at[at.length - 1];
    if (!Number.isNaN(last[5]) && count(pages[pages.length - 1], (k) => k >= hi) === 0) {
      const after = await read(last[5], last[5] + 1);
      if (after.length > 0 && after[0][0] === last[0] + 1) {
        at.push(after[0]);
        pages.push((await parse([after[0]]))[0]);
      }
    }
    // Each page was checked against where the next starts, so they follow on.
    const entries = new Float64Array(pages.reduce((n, p) => n + p.entries.length, 0));
    let o = 0;
    for (const p of pages) {
      entries.set(p.entries, o);
      o += p.entries.length;
    }
    return { entries, first: at[0][0] === 0, toEnd: at.some((p) => Number.isNaN(p[5])) };
  }

  /** The slice of stored channel `c`'s list that axis ticks `[start, end)` need. */
  #sliceOver(c: number, start: number, end: number, signal?: AbortSignal): Promise<Slice> {
    const map = this.#maps[c];
    return this.#slice(c, firstAt(map, Math.floor(start)) - 1, firstAt(map, Math.ceil(end)) + 1, signal);
  }

  /** What an answer over axis ticks `[start, end)` of overview channel `o` carries of the slice. */
  #listed(o: number, s: Slice, start: number, end: number): Listed {
    const c = this.#listedBy(o);
    const map = this.#maps[c];
    const h = this.header.channels[c];
    const e = restrict(s.entries, h.kind === "digital", firstAt(map, Math.floor(start)) - 1, firstAt(map, Math.ceil(end)) + 1);
    const atStart = s.first && e.byteOffset === s.entries.byteOffset;
    if (h.kind === "digital") {
      const gaps: Gap[] = [];
      let start: number | undefined;
      for (let i = 0; i < e.length; i += ENTRY) {
        // A gap from the channel's start says where its level is first known.
        if (atStart && i === 0 && e[0] === this.#starts[c]) start = axisTick(map, e[1]);
        else gaps.push({ from: axisTick(map, e[i]), to: axisTick(map, e[i + 1]), level: e[i + 2] ? 1 : 0 });
      }
      return { gaps, ...(start !== undefined && { start }) };
    }
    const breaks: Break[] = [];
    for (let i = 0; i < e.length; i += ENTRY) breaks.push(breakAt(e, i));
    if (!s.toEnd) return { breaks };
    // The whole list's last run is the slice's last entry's.
    const last = s.entries.length >= ENTRY ? [breakAt(s.entries, s.entries.length - ENTRY)] : [];
    const phase = h.phase && { num: h.phase[0], den: h.phase[1] };
    const runs = { first: h.first, period: { num: h.period[0], den: h.period[1] }, phase, count: h.count, breaks: last, map };
    return { breaks, end: Math.min(this.header.length, h.count === 0 ? axisTick(map, h.first) : placeAfter(runs, h.count - 1)) };
  }

  /** Stored channel `c`'s blocks `refs`, decoded, each checked against the entries of `slice` its
   * ticks need where the channel's list is in pages. */
  #blocks(c: number, refs: Float64Array[], slice: Slice | null, signal?: AbortSignal): Promise<DecodedBlock[]> {
    const map = this.#maps[c];
    const digital = this.header.channels[c].kind === "digital";
    return this.#reader.get(
      refs.map((r) => ({
        key: `${this.#id} f${r[3]}`,
        from: r[3],
        to: r[3] + r[4],
        make: (frame: Uint8Array) => {
          const list = slice ? restrict(slice.entries, digital, firstAt(map, r[0]) - 1, firstAt(map, r[1]) + 1) : undefined;
          return this.#decode(c, r, frame, list);
        },
      })),
      signal,
    );
  }

  #decode(c: number, r: Float64Array, frame: Uint8Array, list: Float64Array | undefined): DecodedBlock {
    const b = this.#rec.decodeBlockAt(c, r, frame, list);
    try {
      const h = this.header.channels[c];
      if (h.kind === "digital") {
        const edges = b.edges()!;
        return { kind: "digital", start: b.start, end: b.end, initial: b.initial() ? 1 : 0, edges, size: 64 + edges.byteLength };
      }
      const values = wide(h) ? b.values64()! : b.values()!;
      const codes = this.#operands.has(c) ? b.codes() : undefined;
      return { kind: "analog", start: b.start, end: b.end, first: b.firstIndex()!, values, ...(codes && { codes }), size: 64 + values.byteLength + (codes?.byteLength ?? 0) };
    } finally {
      (b as WasmBlock).free();
    }
  }

  /** Stored channel `c`'s blocks over `[start, end)`, and the slice of its list they and the span
   * need where it is in pages. */
  async #covered(c: number, start: number, end: number, signal?: AbortSignal): Promise<{ blocks: DecodedBlock[]; slice: Slice | null }> {
    const refs = await this.#blockRefs(c, start, end, signal);
    const slice = this.#paged(c) && refs.length > 0 ? await this.#sliceOver(c, Math.min(start, refs[0][0]), Math.max(end, refs[refs.length - 1][1]), signal) : null;
    return { blocks: await this.#blocks(c, refs, slice, signal), slice };
  }

  /** The span `[start, end)` kept to the capture, at least one tick. */
  #clamp(start: number, end: number): [number, number] {
    const last = Math.max(0, this.header.length - 1);
    const s = Math.min(Math.max(0, Math.floor(start)), last);
    return [s, Math.max(s + 1, Math.min(Math.ceil(end), this.header.length))];
  }

  /** Throws, saying why, for a channel this version can't read: nothing of it is fetched. */
  #readable(channel: number): void {
    const c = this.info.channels[channel];
    if (c?.kind === "unavailable") throw new RangeError(c.why);
  }

  async edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges> {
    this.#readable(channel);
    const h = this.header.channels[channel] as HeaderChannel | undefined;
    if (h?.kind !== "digital") throw new RangeError(`channel ${channel} is not digital`);
    const [s, e] = this.#clamp(start, end);
    const { blocks, slice } = await this.#covered(channel, s, e, signal);
    const listed = slice ? this.#listed(channel, slice, start, end) : undefined;
    const gaps = this.#gapsBy(channel, slice);
    const parts: Float64Array[] = [];
    let level: 0 | 1 = 0;
    blocks.forEach((b, k) => {
      if (b.kind !== "digital") throw new RangeError(`channel ${channel} is not digital`);
      const lo = lowerBound(b.edges, start);
      if (k === 0) level = levelBefore(b, lo, start, gaps);
      parts.push(b.edges.subarray(lo, lowerBound(b.edges, end, lo)));
    });
    const ticks = new Float64Array(parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of parts) {
      ticks.set(p, at);
      at += p.length;
    }
    return { level, ticks, ...(listed && { listed }) };
  }

  /** Every gap of digital channel `c` the first block of a read can end, the one from the
   * channel's start included: from `slice` where the list is in pages. */
  #gapsBy(c: number, slice: Slice | null): readonly Gap[] | null {
    if (!slice) return this.#gaps[c];
    const map = this.#maps[c];
    const gaps: Gap[] = [];
    for (let i = 0; i < slice.entries.length; i += ENTRY) gaps.push({ from: axisTick(map, slice.entries[i]), to: axisTick(map, slice.entries[i + 1]), level: slice.entries[i + 2] ? 1 : 0 });
    return gaps;
  }

  /**
   * `periodStats` of digital channel `channel`'s edges in `[start, end)`, read as `edges` reads
   * them, every block the span meets, but never joined: `PeriodCounter`'s first pass, then its
   * second, over the same blocks again, where the fences are wanted. The blocks go in groups of at
   * most `groupEdges` edges by their first edges' places (a block alone may hold more), a group's
   * reads out together, as `edges` sends a span's; the next group's go out while one is counted.
   * `limits` are for a test.
   */
  async periods(
    channel: number,
    start: number,
    end: number,
    signal?: AbortSignal,
    limits: { distinctAtMost?: number; groupEdges?: number } = {},
  ): Promise<PeriodStats | null> {
    const { distinctAtMost = DISTINCT_AT_MOST, groupEdges = PIECE_EDGES } = limits;
    this.#readable(channel);
    const h = this.header.channels[channel] as HeaderChannel | undefined;
    if (h?.kind !== "digital") throw new RangeError(`channel ${channel} is not digital`);
    const [s, e] = this.#clamp(start, end);
    const refs = await this.#blockRefs(channel, s, e, signal);
    if (refs.length === 0) return null;
    const slice = this.#paged(channel) ? await this.#sliceOver(channel, Math.min(s, refs[0][0]), Math.max(e, refs[refs.length - 1][1]), signal) : null;
    const gaps = this.#gapsBy(channel, slice);
    // A block's edges run from its first edge's place to the next block's, or to the channel's end.
    const after = (k: number) => (k + 1 < refs.length ? refs[k + 1][2] : h.edges);
    const groups: Float64Array[][] = [];
    for (let i = 0; i < refs.length; ) {
      let j = i + 1;
      while (j < refs.length && after(j) - refs[i][2] <= groupEdges) j++;
      groups.push(refs.slice(i, j));
      i = j;
    }
    let counter = null as PeriodCounter | null;
    const pass = async (each: (tick: number) => void) => {
      let next = this.#blocks(channel, groups[0], slice, signal);
      for (let g = 0; g < groups.length; g++) {
        const blocks = await next;
        if (g + 1 < groups.length) {
          next = this.#blocks(channel, groups[g + 1], slice, signal);
          // Awaited with the next group; a throw before then would leave it unhandled.
          next.catch(() => {});
        }
        for (const b of blocks) {
          if (b.kind !== "digital") throw new RangeError(`channel ${channel} is not digital`);
          const lo = lowerBound(b.edges, start);
          counter ??= new PeriodCounter(levelBefore(b, lo, start, gaps), distinctAtMost);
          for (let i = lo, hi = lowerBound(b.edges, end, lo); i < hi; i++) each(b.edges[i]);
        }
      }
    };
    await pass((t) => counter!.edge(t));
    if (counter!.fences()) await pass((t) => counter!.again(t));
    return counter!.stats();
  }

  /** How many of digital channel `channel`'s edges lie in `[start, end)`, from the index: each
   * block's first edge's place among the channel's edges, and the edges of the blocks holding
   * the span's two ends, decoded (FORMAT.md, "Index"). */
  async edgeCount(channel: number, start: number, end: number, signal?: AbortSignal): Promise<number> {
    this.#readable(channel);
    const h = this.header.channels[channel] as HeaderChannel | undefined;
    if (h?.kind !== "digital") throw new RangeError(`channel ${channel} is not digital`);
    const [s, e] = [Math.max(0, Math.ceil(start)), Math.min(this.header.length, Math.ceil(end))];
    if (e <= s) return 0;
    const [before, upTo] = await Promise.all([this.#edgesBefore(channel, h, s, signal), this.#edgesBefore(channel, h, e, signal)]);
    return upTo - before;
  }

  /** How many of digital channel `c`'s edges lie before tick `t`. */
  async #edgesBefore(c: number, h: HeaderDigital, t: number, signal?: AbortSignal): Promise<number> {
    if (t <= 0 || h.edges === 0) return 0;
    if (t >= this.header.length) return h.edges;
    const refs = await this.#blockRefs(c, t, t + 1, signal);
    if (refs.length === 0) throw new Error(`${this.#name}: channel ${c}: no block holds tick ${t}`);
    // A block's third number is its first edge's place among the channel's.
    if (refs[0][0] === t) return refs[0][2];
    const { blocks } = await this.#covered(c, t, t + 1, signal);
    const b = blocks[0];
    if (b?.kind !== "digital") throw new RangeError(`channel ${c} is not digital`);
    return refs[0][2] + lowerBound(b.edges, t);
  }

  async samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    this.#readable(channel);
    const info = this.info.channels[channel];
    if (info?.kind !== "analog") throw new RangeError(`channel ${channel} is not analog`);
    const C = this.header.channels.length;
    const by = this.#listedBy(channel);
    // A channel whose breaks are in pages is placed by those the span needs.
    const slice = this.#paged(by) ? await this.#sliceOver(by, start, end, signal) : null;
    const listed = slice ? this.#listed(channel, slice, start, end) : undefined;
    const place: AnalogChannel = slice ? { ...info, breaks: listed!.breaks } : info;
    const i0 = channelIndexAtOrAfter(place, start);
    const i1 = Math.max(i0, channelIndexAtOrAfter(place, end));
    const h = this.header.channels[channel] as HeaderChannel | undefined;
    // A derived channel's values are f64.
    const Values = !h || (h.kind === "analog" && wide(h)) ? Float64Array : Float32Array;
    let values: Float32Array | Float64Array = new Values(i1 - i0);
    if (i1 > i0) {
      const from = channelSampleTick(place, i0);
      const to = channelSampleTick(place, i1 - 1) + 1;
      if (channel < C) {
        const { blocks } = await this.#covered(channel, from, to, signal);
        for (const b of blocks) {
          if (b.kind !== "analog") throw new RangeError(`channel ${channel} is not analog`);
          const lo = Math.max(i0, b.first);
          const hi = Math.min(i1, b.first + b.values.length);
          if (hi > lo) values.set(b.values.subarray(lo - b.first, hi - b.first), lo - i0);
        }
      } else values = await this.#product(channel - C, from, to, i0, i1, signal);
    }
    // Built here, off the page's thread, so a draw never builds them.
    const n = Math.floor(values.length / SAMPLES_PER_RUN);
    const runs = { min: new Values(n), max: new Values(n) };
    fillRuns(values, runs.min, runs.max, 0, n);
    return { first: i0, values, runs, ...(listed && { listed }) };
  }

  /** Derived channel `d`'s samples `i0..i1`, whose ticks lie in `[from, to)`: its operands'
   * samples at the same indices, through its operation by the format's rule in the decoder. */
  async #product(d: number, from: number, to: number, i0: number, i1: number, signal?: AbortSignal): Promise<Float64Array> {
    const names = this.header.derived![d].operands;
    const operands = names.map((n) => this.header.channels.findIndex((c) => c.name === n));
    const stored = await Promise.all(
      operands.map(async (c) => {
        const h = this.header.channels[c] as HeaderAnalog;
        const { blocks } = await this.#covered(c, from, to, signal);
        const out = h.coding.type === "int" ? new BigInt64Array(i1 - i0) : h.coding.type === "f64" ? new Float64Array(i1 - i0) : new Float32Array(i1 - i0);
        let filled = 0;
        for (const b of blocks) {
          if (b.kind !== "analog") throw new RangeError(`channel ${c} is not analog`);
          const lo = Math.max(i0, b.first);
          const hi = Math.min(i1, b.first + b.values.length);
          if (hi <= lo) continue;
          if (out instanceof BigInt64Array) out.set(b.codes!.subarray(lo - b.first, hi - b.first), lo - i0);
          else out.set(b.values.subarray(lo - b.first, hi - b.first), lo - i0);
          filled += hi - lo;
        }
        if (filled !== i1 - i0) throw new Error(`${this.#name}: ${h.name}'s blocks hold ${filled} of its samples ${i0} to ${i1}`);
        return out;
      }),
    );
    return this.#rec.productValues(d, stored[0], stored[1]);
  }

  /**
   * The coarsest level no coarser than `maxBucket`, or null: the strip, from the footer, or a finer
   * one read from its overview blocks. With no limit it is the strip alone, which carries no
   * `listed`.
   */
  async overview(channel: number, start: number, end: number, maxBucket: number, signal?: AbortSignal): Promise<DigitalOverview | AnalogOverview | null> {
    this.#readable(channel);
    const shifts = this.#levels(channel);
    let li = -1;
    shifts.forEach((sh, i) => {
      if (2 ** sh <= maxBucket) li = i;
    });
    if (li < 0) return null;
    const bucket = 2 ** shifts[li];
    const count = Math.ceil(this.header.length / bucket);
    const c = this.header.channels[channel] as HeaderChannel | undefined;
    const k0 = Math.min(count, Math.max(0, Math.floor(start / bucket)));
    const k1 = Math.min(count, Math.max(k0, Math.ceil(Math.min(end, this.info.channels[channel].end) / bucket)));
    const by = this.#listedBy(channel);
    const listing = maxBucket !== Infinity && this.#paged(by) ? this.#sliceOver(by, start, end, signal) : null;
    // Awaited below, after the buckets: a failure meanwhile is that await's to report.
    listing?.catch(() => undefined);
    // Each source of buckets: [first bucket, arrays].
    let parts: [number, Arrays][];
    if (li === shifts.length - 1) parts = [[0, this.#strip(channel)]];
    else {
      parts = [];
      if (k1 > k0) {
        const b0 = Math.floor(k0 / BLOCK_BUCKETS);
        const b1 = Math.floor((k1 - 1) / BLOCK_BUCKETS) + 1;
        const at = await this.#located(() => this.#rec.levelPages(channel, li, b0, b1), signal);
        const pages = await this.#pages(at, (j) => `${this.#id} l${channel}.${li}.${j}`, (p, b) => this.#rec.levelPage(channel, li, p, b), signal);
        const rows: Float64Array[] = [];
        for (const { entries } of pages) {
          for (let i = 0; i < entries.length; i += ROW) if (entries[i] + entries[i + 4] > b0 && entries[i] < b1) rows.push(entries.subarray(i, i + ROW));
        }
        const decoded = await this.#reader.get(
          rows.map((r) => ({
            key: `${this.#id} o${r[1]}`,
            from: r[1],
            to: r[1] + r[2],
            make: (frame: Uint8Array): DecodedOverview => {
              const level = this.#rec.decodeOverviewAt(channel, li, r, frame);
              try {
                const arrays = this.#arrays(channel, level);
                return { arrays, size: 64 + arrays.reduce((n, a) => n + a.byteLength, 0) };
              } finally {
                level.free();
              }
            },
          })),
          signal,
        );
        // A row stands for `n` blocks alike, each the frame's buckets.
        rows.forEach((r, i) => {
          for (let k = Math.max(r[0], b0); k < Math.min(r[0] + r[4], b1); k++) parts.push([k * BLOCK_BUCKETS, decoded[i].arrays]);
        });
      }
    }
    const slice = await listing;
    const listed = slice ? this.#listed(channel, slice, start, end) : undefined;
    const n = k1 - k0;
    if (c?.kind === "digital") {
      const density = new Uint8Array(n);
      // A bucket no level holds lies outside the channel's kept span, for all this answer knows.
      const duty = new Uint8Array(n).fill(15);
      for (const [first, arrays] of parts) {
        const [d, u] = arrays as Uint8Array[];
        const from = Math.max(k0, first);
        const to = Math.min(k1, first + d.length);
        if (to <= from) continue;
        density.set(d.subarray(from - first, to - first), from - k0);
        duty.set(u.subarray(from - first, to - first), from - k0);
      }
      return { kind: "digital", bucket, start: k0 * bucket, density, duty, ...(listed && { listed }) };
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
    return { kind: "analog", bucket, start: k0 * bucket, min, max, ...(listed && { listed }) };
  }

  /** The annotation bundle's files, by name, read the first time they are asked for and kept in
   * the cache; null for a recording that bundles none. */
  async bundle(signal?: AbortSignal): Promise<Record<string, string> | null> {
    const range = this.#rec.bundleRange();
    if (range.length === 0) return null;
    const [b] = await this.#reader.get(
      [{ key: `${this.#id} a`, from: range[0], to: range[1], make: (b: Uint8Array) => ({ files: JSON.parse(this.#rec.bundleFiles(b)) as Record<string, string>, size: 64 + b.length }) }],
      signal,
    );
    return b.files;
  }

  /** The parts read so far, by row. */
  readonly #parts = new Map<number, PartRead>();
  #bundleRead: Promise<BundleRead | null> | null = null;

  /** The bundle as reading annotations needs it, read the first time asked; null without one. */
  #readBundle(): Promise<BundleRead | null> {
    // Read once for every caller, so none's abort ends another's read: it is one range.
    this.#bundleRead ??= (async () => {
      const files = await this.bundle();
      if (!files) return null;
      const hand = Object.keys(files).filter((n) => n.endsWith(".toml"));
      if (hand.length !== 1) throw new Error(`${this.#name}: the bundled sidecar has ${hand.length} TOML files, not one`);
      const { json } = handJson(files[hand[0]], hand[0]);
      const names = Object.keys(files).filter((n) => n !== hand[0]);
      const texts = names.map((n) => files[n]);
      const refs = new Set<string>();
      const entries: unknown = (JSON.parse(json) as { annotation?: unknown }).annotation;
      for (const a of Array.isArray(entries) ? entries : []) if (a && typeof a === "object" && typeof a.ref === "string") refs.add(a.ref);
      // The files' order and which are kept come from resolving the file apart, refs aside.
      const apart = JSON.parse(this.#rec.resolveBundled(json, names, texts, "[]")) as { files: { name: string; kept: boolean }[] };
      const rank = new Map(apart.files.map((f, i) => [f.name, f.kept ? i + 1 : null]));
      const ranks = new Map<string, number | null>();
      for (const name of names) {
        const source = generatorOf(files[name]);
        if (source === null) continue;
        if (ranks.has(source)) throw new Error(`${this.#name}: the annotation bundle holds two files of generator ${JSON.stringify(source)}`);
        ranks.set(source, rank.get(name) ?? null);
      }
      return { hand: json, refs, names, texts, ranks };
    })();
    this.#bundleRead.catch(() => (this.#bundleRead = null));
    return this.#bundleRead;
  }

  /**
   * The bundled annotations as a reader that reads a range at a time has them (FORMAT.md,
   * "Annotations"): the hand-written file's, and the lines of every part read so far, with those
   * of the parts whose span meets ticks `span` read now; of every part where `span` is null, and
   * of none more where it is empty. Null for a recording that bundles none. A part that repeats
   * an id a part read before it in the parts' order gave is left out, so a part not read can leave
   * a repeat in.
   */
  async annotations(span: readonly [number, number] | null, signal?: AbortSignal): Promise<AnnotationsRead | null> {
    const b = await this.#readBundle();
    if (!b) return null;
    // Rows may pass the length, and start before 0 only in a crafted file: every tick a double holds.
    const [lo, hi] = span ?? [-(2 ** 53), 2 ** 53];
    if (hi <= lo) return this.#gathered(b);
    const at = await this.#located(() => this.#rec.partPages(lo, hi), signal);
    const pages = await this.#pages(at, (j) => `${this.#id} p${j}`, (p, bytes) => this.#rec.partPage(p, bytes), signal);
    const rows: Float64Array[] = [];
    for (const { entries } of pages) {
      for (let i = 0; i < entries.length; i += PART) {
        // A part meets the span where it starts before the span's end and ends at or after its start.
        if (entries[i + 4] < hi && entries[i + 5] >= lo && !this.#parts.has(entries[i])) rows.push(entries.subarray(i, i + PART));
      }
    }
    const read = await this.#reader.get(
      rows.map((r) => ({
        key: `${this.#id} a${r[0]}`,
        from: r[1],
        to: r[1] + r[2],
        make: (frame: Uint8Array): PartRead => ({ ...(JSON.parse(this.#rec.partAnnotations(r, frame)) as Omit<PartRead, "size">), size: 64 + 2 * r[3] }),
      })),
      signal,
    );
    rows.forEach((r, i) => this.#parts.set(r[0], read[i]));
    return this.#gathered(b);
  }

  /** The annotations of the bundle and the parts read so far. */
  #gathered(b: BundleRead): AnnotationsRead {
    const problems: string[] = [];
    const generated: { order: [number, number, number]; a: CoreAnnotation }[] = [];
    const claimed = new Set<string>();
    for (const row of [...this.#parts.keys()].sort((x, y) => x - y)) {
      const part = this.#parts.get(row)!;
      const rank = b.ranks.get(part.source);
      if (rank === undefined) throw new Error(`${this.#name}: an annotation part of generator ${JSON.stringify(part.source)}, which has no file in the bundle`);
      part.lines.forEach((line, n) => {
        if (line.past) {
          problems.push(line.left!);
          return;
        }
        if (line.id != null) {
          const id = `${part.source}\n${line.id}`;
          if (claimed.has(id)) {
            problems.push(`${part.source}/${line.id}: an id its generator has given already`);
            return;
          }
          claimed.add(id);
        }
        // A file whose first line breaks a rule: the bundle's problems name it.
        if (rank === null) return;
        if (line.annotation) generated.push({ order: [rank, row, n], a: line.annotation });
        else problems.push(line.left!);
      });
    }
    generated.sort((x, y) => x.order[0] - y.order[0] || x.order[1] - y.order[1] || x.order[2] - y.order[2]);
    const complete = this.#parts.size === (this.header.annotation_parts ?? 0);
    // A ref resolves once every part is read: a part not read can hold its target's id first.
    const targets = complete ? generated.filter((g) => b.refs.has(g.a.id)).map((g) => g.a) : [];
    const apart = JSON.parse(this.#rec.resolveBundled(b.hand, b.names, b.texts, JSON.stringify(targets))) as {
      annotations: { place: number; annotation: CoreAnnotation }[];
      problems: string[];
    };
    if (complete) problems.push(...apart.problems);
    const hand = apart.annotations.sort((x, y) => x.place - y.place).map((h) => h.annotation);
    return { list: [...hand, ...generated.map((g) => g.a)], problems, complete };
  }

  /** Resolves a sidecar (the hand-written file as JSON, and the files it includes) against this recording. */
  resolveAnnotations(hand: string, names: string[], texts: string[], recordingFile: string | null): string {
    return this.#rec.resolveAnnotations(hand, names, texts, recordingFile);
  }
}
