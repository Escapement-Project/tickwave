// Where a recording's tables and frames lie, read from its bytes by FORMAT.md's layout ("Index",
// "Footer") without the decoder, so a test can say which bytes a reader should fetch. It checks
// nothing a reader checks: the fixtures are well formed.

/** A directory or top-index entry: where a page starts, and its record. */
export interface Entry {
  readonly first: number;
  readonly number: number;
  readonly offset: number;
  readonly length: number;
}

export interface Table {
  readonly depth: number;
  readonly entries: readonly Entry[];
}

export interface ChannelTables {
  readonly blocks: Table | null;
  readonly levels: readonly Table[];
  readonly list: Table | null;
}

export interface Layout {
  readonly footer: number;
  readonly header: { length: number; channels: { name: string; kind: string; overview_blocks?: unknown[]; gap_count?: number; break_count?: number }[]; derived?: unknown[] };
  readonly bundle: [number, number] | null;
  readonly channels: readonly ChannelTables[];
}

const u32 = (b: Uint8Array, at: number) => new DataView(b.buffer, b.byteOffset + at, 4).getUint32(0, true);
const u64 = (b: Uint8Array, at: number) => Number(new DataView(b.buffer, b.byteOffset + at, 8).getBigUint64(0, true));
const i64 = (b: Uint8Array, at: number) => Number(new DataView(b.buffer, b.byteOffset + at, 8).getBigInt64(0, true));

/** The trailer, the footer's header and its top index. */
export function layout(file: Uint8Array): Layout {
  const footer = u64(file, file.length - 24);
  // A record: kind, payload length, payload, check.
  const payload = footer + 5;
  const headerLen = u32(file, payload);
  const header = JSON.parse(new TextDecoder().decode(file.subarray(payload + 8, payload + 8 + headerLen))) as Layout["header"];
  let at = payload + 8 + headerLen;
  const bundleAt = u64(file, at);
  const bundleLen = u32(file, at + 8);
  at += 12;
  const table = (): Table => {
    const depth = file[at];
    const e = file[at + 1];
    at += 2;
    const entries: Entry[] = [];
    for (let i = 0; i < e; i++, at += 28) entries.push({ first: i64(file, at), number: u64(file, at + 8), offset: u64(file, at + 16), length: u32(file, at + 24) });
    return { depth, entries };
  };
  const channels: ChannelTables[] = [];
  const all = [...header.channels, ...(header.derived ?? []).map((d) => ({ ...(d as object), kind: "derived" }) as Layout["header"]["channels"][number])];
  for (const c of all) {
    at += 8;
    const blocks = c.kind === "derived" ? null : table();
    const levels = (c.overview_blocks ?? []).map(() => table());
    const list = c.gap_count !== undefined || c.break_count !== undefined ? table() : null;
    channels.push({ blocks, levels, list });
  }
  return { footer, header, bundle: bundleLen ? [bundleAt, bundleAt + bundleLen] : null, channels };
}

/** A page record's entries, each `size` bytes, after its 19-byte head. */
function page(file: Uint8Array, e: Entry, size: number): Uint8Array[] {
  const payload = e.offset + 5;
  const count = u32(file, payload + 15);
  const out: Uint8Array[] = [];
  for (let i = 0; i < count; i++) out.push(file.subarray(payload + 19 + i * size, payload + 19 + (i + 1) * size));
  return out;
}

/** The depth-0 pages of `t`, directories read through. */
export function leaves(file: Uint8Array, t: Table): Entry[] {
  let entries = [...t.entries];
  for (let d = t.depth; d > 0; d--) {
    entries = entries.flatMap((e) => page(file, e, 28).map((b) => ({ first: i64(b, 0), number: u64(b, 8), offset: u64(b, 16), length: u32(b, 24) })));
  }
  return entries;
}

/** The directory pages of `t`. */
export function directories(file: Uint8Array, t: Table): Entry[] {
  const out: Entry[] = [];
  let entries = [...t.entries];
  for (let d = t.depth; d > 0; d--) {
    out.push(...entries);
    entries = entries.flatMap((e) => page(file, e, 28).map((b) => ({ first: i64(b, 0), number: u64(b, 8), offset: u64(b, 16), length: u32(b, 24) })));
  }
  return out;
}

/** A block row: its start tick and its frame's byte range. */
export interface BlockRow {
  readonly start: number;
  readonly from: number;
  readonly to: number;
}

/** Every block row of a channel's block table, in order. */
export function blockRows(file: Uint8Array, t: Table): BlockRow[] {
  return leaves(file, t).flatMap((e) =>
    page(file, e, 28).map((b) => {
      const from = u64(b, 8);
      return { start: i64(b, 0), from, to: from + u32(b, 16) };
    }),
  );
}

/** An overview row: the first block it stands for, how many, and its frame's byte range. */
export interface LevelRow {
  readonly k: number;
  readonly n: number;
  readonly from: number;
  readonly to: number;
}

/** Every row of one overview level's table, in order. */
export function levelRows(file: Uint8Array, t: Table): LevelRow[] {
  let k = 0;
  return leaves(file, t).flatMap((e) =>
    page(file, e, 20).map((b) => {
      const from = u64(b, 0);
      const n = u32(b, 16);
      const row = { k, n, from, to: from + u32(b, 8) };
      k += n;
      return row;
    }),
  );
}

/** A byte range as `MemoryBytes.reads` holds one. */
export const range = (e: Entry): [number, number] => [e.offset, e.offset + e.length];
