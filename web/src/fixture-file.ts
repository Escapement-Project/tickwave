// Reads the dev fixture files that scripts/extract-fixtures.ts writes. The layout is in
// web/README.md, "Fixture files". Not part of tickwave's format: the decoder in crates/format
// replaces this once it exists.

import type { MemoryAnalog, MemoryDigital, MemoryRecording } from "./memory-source.ts";

export const FIXTURE_MAGIC = "tkwavefx";
export const FIXTURE_FORMAT = "tickwave-fixture/1";

export interface FixtureHeader {
  format: string;
  title: string;
  timebase: [number, number];
  length: number;
  channels: FixtureChannel[];
  source?: unknown;
}

export type FixtureChannel =
  | {
      kind: "digital";
      name: string;
      end: number;
      initial: 0 | 1;
      count: number;
      bytes: number;
    }
  | {
      kind: "analog";
      name: string;
      end: number;
      unit: string;
      scale: number;
      offset: number;
      first: number;
      period: [number, number];
      count: number;
      bytes: number;
    };

async function gunzip(bytes: Uint8Array): Promise<Uint8Array> {
  const stream = new Blob([bytes as Uint8Array<ArrayBuffer>]).stream().pipeThrough(new DecompressionStream("gzip"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

/** Accepts the file as stored (gzip) or already decompressed by the server. */
export async function readFixture(file: ArrayBuffer | Uint8Array): Promise<MemoryRecording> {
  let bytes = file instanceof Uint8Array ? file : new Uint8Array(file);
  const stored = bytes.length;
  if (bytes[0] === 0x1f && bytes[1] === 0x8b) bytes = await gunzip(bytes);
  const magic = new TextDecoder().decode(bytes.subarray(0, 8));
  if (magic !== FIXTURE_MAGIC) throw new Error("not a tickwave fixture file");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const headerLength = view.getUint32(8, true);
  const header = JSON.parse(new TextDecoder().decode(bytes.subarray(12, 12 + headerLength))) as FixtureHeader;
  if (header.format !== FIXTURE_FORMAT) throw new Error(`unknown fixture format ${header.format}`);
  let at = 12 + headerLength;
  const channels: (MemoryDigital | MemoryAnalog)[] = [];
  for (const c of header.channels) {
    const payload = bytes.subarray(at, at + c.bytes);
    at += c.bytes;
    if (c.kind === "digital") {
      const ticks = decodeIntervals(payload, c.count);
      channels.push({ kind: "digital", name: c.name, end: c.end, initial: c.initial, ticks });
    } else {
      // The file holds integer codes; the source hands out values.
      const values = decodeDeltas(payload, c.count);
      for (let i = 0; i < values.length; i++) values[i] = values[i] * c.scale + c.offset;
      channels.push({
        kind: "analog",
        name: c.name,
        end: c.end,
        unit: c.unit,
        resolution: c.scale,
        first: c.first,
        period: { num: c.period[0], den: c.period[1] },
        values,
      });
    }
  }
  if (at !== bytes.length) throw new Error("fixture length does not match its header");
  return {
    title: header.title,
    timebase: { num: header.timebase[0], den: header.timebase[1] },
    length: header.length,
    bytes: stored,
    channels,
  };
}

// LEB128, decoded with multiplication rather than shifts so a value can use all 53 bits. `zigzag`
// maps 0, 1, 2, 3, ... to 0, -1, 1, -2, ...; each value adds to the running total.
function decodeRunning(bytes: Uint8Array, count: number, zigzag: boolean): Float64Array {
  const out = new Float64Array(count);
  let i = 0;
  let total = 0;
  let value = 0;
  let scale = 1;
  for (let k = 0; k < bytes.length; k++) {
    const b = bytes[k];
    value += (b & 0x7f) * scale;
    if (b & 0x80) {
      scale *= 128;
      continue;
    }
    if (i === count) throw new Error("more values than the header says");
    total += zigzag ? (value % 2 === 0 ? value / 2 : -(value + 1) / 2) : value;
    out[i++] = total;
    value = 0;
    scale = 1;
  }
  if (scale !== 1) throw new Error("truncated LEB128");
  if (i !== count) throw new Error("fewer values than the header says");
  return out;
}

/** The first edge's tick, then each interval to the next edge. */
export function decodeIntervals(bytes: Uint8Array, count: number): Float64Array {
  return decodeRunning(bytes, count, false);
}

/** The first code, then each difference to the next. */
export function decodeDeltas(bytes: Uint8Array, count: number): Float64Array {
  return decodeRunning(bytes, count, true);
}
