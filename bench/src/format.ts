// A reader for the tickwave recording format, written from FORMAT.md (version 1.0) alone, as a
// second implementation of the spec. The zstd inflate is the caller's: every frame's bytes go out
// through `frame()`/`overviewFrame` and come back decoded, so the same reader serves a synchronous
// inflate (fzstd) and an asynchronous one (DecompressionStream).

export const PREAMBLE_LEN = 32;
export const FIRST_FETCH = 65536;
const MAGIC = [0x89, 0x54, 0x4b, 0x57, 0x0d, 0x0a, 0x1a, 0x0a];
const TWO32 = 4294967296;

export class FormatError extends Error {}

function fail(msg: string): never {
  throw new FormatError(msg);
}

function u64(v: DataView, at: number): number {
  const lo = v.getUint32(at, true);
  const hi = v.getUint32(at + 4, true);
  if (hi >= 0x200000) fail(`u64 at ${at} is not below 2^53`);
  return hi * TWO32 + lo;
}

function i64(v: DataView, at: number): number {
  const lo = v.getUint32(at, true);
  const hi = v.getInt32(at + 4, true);
  if (hi >= 0x200000 || hi < -0x200000) fail(`i64 at ${at} is outside ±2^53`);
  return hi * TWO32 + lo;
}

function isInt(x: unknown): x is number {
  return typeof x === "number" && Number.isSafeInteger(x);
}

export interface Preamble {
  major: number;
  minor: number;
  headerLen: number;
  indexLen: number;
  headLen: number;
  fileLen: number;
}

export function parsePreamble(b: Uint8Array): Preamble {
  if (b.length < PREAMBLE_LEN) fail("shorter than the preamble");
  for (let i = 0; i < MAGIC.length; i++) if (b[i] !== MAGIC[i]) fail("not a tickwave recording");
  const v = new DataView(b.buffer, b.byteOffset, PREAMBLE_LEN);
  const major = v.getUint16(8, true);
  if (major !== 1) fail(`major version ${major} is not 1`);
  return {
    major,
    minor: v.getUint16(10, true),
    headerLen: v.getUint32(12, true),
    indexLen: v.getUint32(16, true),
    headLen: v.getUint32(20, true),
    fileLen: u64(v, 24),
  };
}

/** How many bytes the head needs: 32 until the preamble is in hand. */
export function headLength(b: Uint8Array): number {
  return b.length < PREAMBLE_LEN ? PREAMBLE_LEN : parsePreamble(b).headLen;
}

export type Coding =
  | { type: "f32" }
  | { type: "codes"; bits: number; scale: number; offset: number };

export type ChannelInfo =
  | { kind: "digital"; name: string; initial: boolean; end: number; edges: number }
  | {
      kind: "analog";
      name: string;
      unit: string;
      first: number;
      period: [number, number];
      count: number;
      coding: Coding;
    };

export interface Header {
  timebase: [number, number];
  length: number;
  channels: ChannelInfo[];
  blocks: number;
  overview: number[];
  overview_blocks?: { shift: number; blocks: number }[];
  [k: string]: unknown;
}

export interface OverviewRow {
  offset: number;
  compressed: number;
  decoded: number;
  shift: number;
  /** The block's first bucket, and how many it holds. */
  first: number;
  buckets: number;
}

/** Buckets a level may have: in the head, and in overview blocks. */
const HEAD_BUCKETS = 2 ** 17;
const BLOCK_BUCKETS = 2 ** 26;
/** Buckets an overview block holds; a level's last block holds the rest. */
const OVERVIEW_BLOCK = 65536;

export interface Row {
  start: number;
  offset: number;
  compressed: number;
  decoded: number;
  /** Per channel: digital, the edge count (bits 0-30) and start level (bit 31); analog, samples. */
  counts: Uint32Array;
}

export type OverviewChannel =
  | { kind: "digital"; level: Uint8Array; change: Uint8Array }
  | { kind: "analog"; min: Float32Array; max: Float32Array };

export interface OverviewLevel {
  shift: number;
  buckets: number;
  channels: OverviewChannel[];
}

export type BlockChannel =
  | { kind: "digital"; initial: boolean; edges: Float64Array }
  | { kind: "analog"; firstIndex: number; values: Float32Array };

export interface Block {
  start: number;
  /** Exclusive; `Infinity` for the last block. */
  end: number;
  channels: BlockChannel[];
}

function checkHeader(h: any): Header {
  if (typeof h !== "object" || h === null) fail("header is not an object");
  const tb = h.timebase;
  if (!Array.isArray(tb) || tb.length !== 2 || !isInt(tb[0]) || !isInt(tb[1]) || tb[0] <= 0 || tb[1] <= 0)
    fail("timebase: two terms above zero");
  if (!isInt(h.length) || h.length < 0) fail("length: a tick below 2^53");
  if (!isInt(h.blocks) || h.blocks < 1) fail("blocks");
  // Channels before overview levels: a wrong `length` also breaks the levels' bucket counts, and
  // the channels' ticks name it more directly.
  if (!Array.isArray(h.channels)) fail("channels");
  for (const c of h.channels) {
    if (typeof c?.name !== "string") fail("channel name");
    if (c.kind === "digital") {
      if (typeof c.initial !== "boolean" || !isInt(c.end) || !isInt(c.edges) || c.edges < 0) fail(`digital channel ${c.name}`);
      if (c.end < 0 || c.end > h.length) fail(`channel ${c.name}: end ${c.end} is outside 0..=${h.length}`);
    } else if (c.kind === "analog") {
      const p = c.period;
      if (!isInt(c.first) || !isInt(c.count) || c.count < 0) fail(`analog channel ${c.name}`);
      if (!Array.isArray(p) || p.length !== 2 || !isInt(p[0]) || !isInt(p[1])) fail(`channel ${c.name}: period`);
      if (p[0] <= 0 || p[1] <= 0) fail(`channel ${c.name}: period [${p}] has a term that is not above zero`);
      // The last sample sits at first + floor((count - 1) * period), exactly, in BigInt.
      const last = c.count === 0 ? c.first : BigInt(c.first) + (BigInt(c.count - 1) * BigInt(p[0])) / BigInt(p[1]);
      if (c.first < 0 || BigInt(last) > BigInt(h.length)) fail(`channel ${c.name}: samples leave 0..=${h.length}`);
      const k = c.coding?.type;
      if (k === "codes") {
        const b = c.coding.bits;
        if (!isInt(b) || b < 1 || b > 16) fail(`channel ${c.name}: bits ${b} is outside 1..=16`);
        if (typeof c.coding.scale !== "number" || typeof c.coding.offset !== "number") fail(`channel ${c.name}: coding`);
      } else if (k !== "f32") fail(`channel ${c.name}: coding ${k}`);
    } else fail(`channel kind ${c.kind}`);
  }
  if (!Array.isArray(h.overview) || h.overview.length === 0 || !h.overview.every((s: unknown) => isInt(s) && s >= 0 && s < 64))
    fail("overview: the head holds at least one level");
  const ob = h.overview_blocks ?? [];
  if (!Array.isArray(ob) || !ob.every((l: any) => isInt(l?.shift) && l.shift >= 0 && l.shift < 64 && isInt(l.blocks) && l.blocks >= 1))
    fail("overview_blocks");
  for (const s of h.overview) if (buckets(h.length, s) > HEAD_BUCKETS) fail(`overview level ${s} has too many buckets for the head`);
  for (const l of ob) {
    const n = buckets(h.length, l.shift);
    if (n > BLOCK_BUCKETS) fail(`overview level ${l.shift} has too many buckets for overview blocks`);
    if (l.blocks !== Math.ceil(n / OVERVIEW_BLOCK)) fail(`overview level ${l.shift}: ${l.blocks} blocks for ${n} buckets`);
  }
  return h as Header;
}

export class Head {
  readonly preamble: Preamble;
  readonly header: Header;
  readonly rows: Row[];
  readonly overviewRows: OverviewRow[];
  /** `firstIndex[block][channel]`: an analog channel's index of the block's first sample. */
  readonly firstIndex: number[][];
  /** The overview's zstd frame and its decoded size, for the caller to inflate into `attachOverview`. */
  readonly overviewFrame: Uint8Array;
  readonly overviewLen: number;
  overview: OverviewLevel[] | null = null;

  /** Parses everything in the head but the overview. `b` holds at least `head_len` bytes. */
  constructor(b: Uint8Array) {
    const p = parsePreamble(b);
    this.preamble = p;
    const indexAt = PREAMBLE_LEN + p.headerLen;
    const overviewAt = indexAt + p.indexLen;
    if (overviewAt >= p.headLen) fail("head_len leaves no room for the overview");
    if (p.headLen > p.fileLen) fail("head_len past file_len");
    if (b.length < p.headLen) fail(`need ${p.headLen} bytes of head, have ${b.length}`);

    const text = new TextDecoder("utf-8", { fatal: true }).decode(b.subarray(PREAMBLE_LEN, indexAt));
    const h = checkHeader(JSON.parse(text));
    this.header = h;
    const nch = h.channels.length;

    const rowLen = 24 + 4 * nch;
    const levels = h.overview_blocks ?? [];
    const overviewBlocks = levels.reduce((a, l) => a + l.blocks, 0);
    if (p.indexLen !== h.blocks * rowLen + 16 * overviewBlocks) fail("index_len is not a row a block and an overview block");
    const v = new DataView(b.buffer, b.byteOffset + indexAt, p.indexLen);
    const rows: Row[] = [];
    const firstIndex: number[][] = [];
    const running = new Array<number>(nch).fill(0);
    let offset = p.headLen;
    for (let i = 0; i < h.blocks; i++) {
      const at = i * rowLen;
      const counts = new Uint32Array(nch);
      for (let c = 0; c < nch; c++) counts[c] = v.getUint32(at + 24 + 4 * c, true);
      const row: Row = {
        start: i64(v, at),
        offset: u64(v, at + 8),
        compressed: v.getUint32(at + 16, true),
        decoded: v.getUint32(at + 20, true),
        counts,
      };
      if (i === 0 ? row.start !== 0 : row.start <= rows[i - 1].start) fail(`block ${i} start out of order`);
      if (row.start > h.length) fail(`block ${i} starts past the end`);
      if (row.offset !== offset) fail(`block ${i} is not where the one before it ends`);
      offset += row.compressed;
      firstIndex.push(running.slice());
      for (let c = 0; c < nch; c++) running[c] += h.channels[c].kind === "digital" ? counts[c] & 0x7fffffff : counts[c];
      rows.push(row);
    }
    const overviewRows: OverviewRow[] = [];
    let at = h.blocks * rowLen;
    for (const l of levels) {
      const n = buckets(h.length, l.shift);
      for (let k = 0; k < l.blocks; k++, at += 16) {
        const first = k * OVERVIEW_BLOCK;
        const count = Math.min(OVERVIEW_BLOCK, n - first);
        const row: OverviewRow = {
          offset: u64(v, at),
          compressed: v.getUint32(at + 8, true),
          decoded: v.getUint32(at + 12, true),
          shift: l.shift,
          first,
          buckets: count,
        };
        if (row.offset !== offset) fail(`overview block ${overviewRows.length} is not where the one before it ends`);
        offset += row.compressed;
        overviewRows.push(row);
      }
    }
    if (offset !== p.fileLen) fail("blocks and overview blocks do not end at file_len");
    this.overviewRows = overviewRows;
    h.channels.forEach((c, k) => {
      const total = c.kind === "digital" ? c.edges : c.count;
      if (running[k] !== total) fail(`channel ${c.name}: the index counts ${running[k]}, the header ${total}`);
      if (c.kind !== "digital") return;
      let level = c.initial;
      for (let i = 0; i < rows.length; i++) {
        const word = rows[i].counts[k];
        if ((word >>> 31 === 1) !== level) fail(`channel ${c.name}: block ${i}'s level bit does not follow from initial and the edge counts`);
        if ((word & 0x7fffffff) % 2 === 1) level = !level;
      }
    });
    this.rows = rows;
    this.firstIndex = firstIndex;

    this.overviewFrame = b.subarray(overviewAt, p.headLen);
    this.overviewLen = h.overview.reduce((a, s) => a + levelLen(h.channels, buckets(h.length, s)), 0);
  }

  attachOverview(d: Uint8Array): void {
    if (d.length !== this.overviewLen) fail("overview's decoded size");
    const levels: OverviewLevel[] = [];
    let at = 0;
    for (const shift of this.header.overview) {
      const n = buckets(this.header.length, shift);
      levels.push({ shift, buckets: n, channels: unpackLevel(this.header.channels, d, at, n) });
      at += levelLen(this.header.channels, n);
    }
    this.overview = levels;
  }

  /** Overview block j's byte range in the file, `[offset, end)`. */
  overviewBlockRange(j: number): [number, number] {
    const r = this.overviewRows[j];
    return [r.offset, r.offset + r.compressed];
  }

  /** Decodes overview block j from its inflated payload: one level's layout, for its own buckets. */
  decodeOverviewPayload(j: number, d: Uint8Array): { shift: number; first: number; buckets: number; channels: OverviewChannel[] } {
    const r = this.overviewRows[j];
    const need = levelLen(this.header.channels, r.buckets);
    if (r.decoded !== need) fail(`overview block ${j}: the index says ${r.decoded} bytes, ${r.buckets} buckets take ${need}`);
    if (d.length !== r.decoded) fail(`overview block ${j}: decoded ${d.length} bytes, the index says ${r.decoded}`);
    return { shift: r.shift, first: r.first, buckets: r.buckets, channels: unpackLevel(this.header.channels, d, 0, r.buckets) };
  }

  get blockCount(): number {
    return this.rows.length;
  }

  /** Block i's byte range in the file, `[offset, end)`. */
  blockRange(i: number): [number, number] {
    const r = this.rows[i];
    return [r.offset, r.offset + r.compressed];
  }

  /** Decodes block i from its inflated payload, which must be exactly the index's decoded length. */
  decodePayload(i: number, d: Uint8Array): Block {
    const row = this.rows[i];
    const chans = this.header.channels;
    const nch = chans.length;
    if (d.length !== row.decoded) fail(`block ${i}: decoded ${d.length} bytes, the index says ${row.decoded}`);
    if (d.length < 4 * nch) fail(`block ${i}: shorter than its section lengths`);
    const last = i === this.rows.length - 1;
    const end = last ? Infinity : this.rows[i + 1].start;
    const limit = last ? this.header.length : end - 1;
    const v = new DataView(d.buffer, d.byteOffset, 4 * nch);
    let at = 4 * nch;
    const out: BlockChannel[] = [];
    for (let c = 0; c < nch; c++) {
      const len = v.getUint32(4 * c, true);
      const sectionEnd = at + len;
      if (sectionEnd > d.length) fail(`block ${i} channel ${c}: section runs past the payload`);
      const info = chans[c];
      if (info.kind === "digital") {
        const word = row.counts[c];
        const edges = intervals(d, at, sectionEnd, word & 0x7fffffff, row.start, Math.min(limit, info.end), i, c);
        if (edges.length > 0 && edges[0] < 1) fail(`block ${i} channel ${c}: an edge at tick 0`);
        out.push({ kind: "digital", initial: word >>> 31 === 1, edges });
      } else {
        const n = row.counts[c];
        const values =
          info.coding.type === "f32"
            ? (len === 4 * n ? f32s(d, at, n) : fail(`block ${i} channel ${c}: ${len} bytes for ${n} f32`))
            : len === 2 * n
              ? codes(d, at, n, info.coding)
              : fail(`block ${i} channel ${c}: ${len} bytes for ${n} codes`);
        out.push({ kind: "analog", firstIndex: this.firstIndex[i][c], values });
      }
      at = sectionEnd;
    }
    if (at !== d.length) fail(`block ${i}: ${d.length - at} bytes after the last section`);
    return { start: row.start, end, channels: out };
  }
}

export function buckets(length: number, shift: number): number {
  return Math.floor(length / 2 ** shift) + 1;
}

/** Decoded bytes of one overview level of n buckets. */
function levelLen(channels: ChannelInfo[], n: number): number {
  return channels.reduce((a, c) => a + (c.kind === "digital" ? 2 * Math.ceil(n / 8) : 8 * n), 0);
}

function unpackLevel(channels: ChannelInfo[], d: Uint8Array, at: number, n: number): OverviewChannel[] {
  return channels.map((c) => {
    if (c.kind === "digital") {
      const m = Math.ceil(n / 8);
      const level = d.slice(at, at + m);
      const change = d.slice(at + m, at + 2 * m);
      at += 2 * m;
      return { kind: "digital", level, change };
    }
    const min = f32s(d, at, n);
    const max = f32s(d, at + 4 * n, n);
    at += 8 * n;
    return { kind: "analog", min, max };
  });
}

const littleEndian = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;

function f32s(d: Uint8Array, at: number, n: number): Float32Array {
  if (littleEndian) return new Float32Array(d.slice(at, at + 4 * n).buffer);
  const v = new DataView(d.buffer, d.byteOffset + at, 4 * n);
  const out = new Float32Array(n);
  for (let k = 0; k < n; k++) out[k] = v.getFloat32(4 * k, true);
  return out;
}

function codes(d: Uint8Array, at: number, n: number, k: { bits: number; scale: number; offset: number }): Float32Array {
  const out = new Float32Array(n);
  const top = 2 ** k.bits;
  // Two byte planes: every code's low byte, then every high byte.
  const high = at + n;
  for (let j = 0; j < n; j++) {
    const code = d[at + j] | (d[high + j] << 8);
    if (code >= top) fail(`code ${code} is wider than ${k.bits} bits`);
    // In f64, unfused, then rounded to f32 on the store.
    out[j] = k.offset + code * k.scale;
  }
  return out;
}

// LEB128 intervals into absolute ticks. Each interval is held below 2^53 (at most 8 bytes, the
// eighth carrying 4 bits), and ticks only grow, so checking the last tick against `limit` also
// catches a running sum that left the range doubles hold exactly.
function intervals(
  d: Uint8Array,
  at: number,
  end: number,
  n: number,
  start: number,
  limit: number,
  block: number,
  channel: number,
): Float64Array {
  const edges = new Float64Array(n);
  let p = at;
  let t = start;
  for (let k = 0; k < n; k++) {
    if (p >= end) fail(`block ${block} channel ${channel}: ${k} of ${n} edges`);
    let b = d[p++];
    let x: number;
    if (b < 0x80) {
      x = b;
    } else {
      x = b & 0x7f;
      let m = 128;
      for (;;) {
        if (p >= end) fail(`block ${block} channel ${channel}: interval runs past its section`);
        b = d[p++];
        if (m === 2 ** 49) {
          if (b > 0x0f) fail(`block ${block} channel ${channel}: interval not below 2^53`);
          x += b * m;
          break;
        }
        x += (b & 0x7f) * m;
        if (b < 0x80) break;
        m *= 128;
      }
    }
    if (x === 0 && k > 0) fail(`block ${block} channel ${channel}: a zero interval after the first`);
    t += x;
    edges[k] = t;
  }
  if (p !== end) fail(`block ${block} channel ${channel}: ${end - p} bytes after the last edge`);
  if (n > 0 && t > limit) fail(`block ${block} channel ${channel}: edge at ${t} is past the block`);
  return edges;
}
