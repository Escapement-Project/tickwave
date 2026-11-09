// The bytes every decoder's output is hashed over, identical to `canonical()` in bench/native:
//   i64 block start
//   per channel, digital: u8 0, u8 initial level, u64 n, then n edge ticks as i64
//                analog:  u8 1, u64 first sample's index, u64 n, then n values' f32 bits as u32
// All little-endian. Hashed with SHA-256.

import type { Block, OverviewChannel } from "./format.ts";

const TWO32 = 4294967296;

function put64(v: DataView, at: number, x: number): void {
  if (!Number.isSafeInteger(x) || x < 0) throw new Error(`canonical: ${x} is not a tick`);
  v.setUint32(at, x >>> 0, true);
  v.setUint32(at + 4, Math.floor(x / TWO32), true);
}

export function canonical(b: Block): Uint8Array {
  let len = 8;
  for (const c of b.channels) len += c.kind === "digital" ? 10 + 8 * c.edges.length : 17 + 4 * c.values.length;
  const out = new Uint8Array(len);
  const v = new DataView(out.buffer);
  put64(v, 0, b.start);
  let at = 8;
  for (const c of b.channels) {
    if (c.kind === "digital") {
      out[at] = 0;
      out[at + 1] = c.initial ? 1 : 0;
      put64(v, at + 2, c.edges.length);
      at += 10;
      for (let k = 0; k < c.edges.length; k++, at += 8) put64(v, at, c.edges[k]);
    } else {
      out[at] = 1;
      put64(v, at + 1, c.firstIndex);
      put64(v, at + 9, c.values.length);
      at += 17;
      out.set(new Uint8Array(c.values.buffer, c.values.byteOffset, 4 * c.values.length), at);
      at += 4 * c.values.length;
    }
  }
  return out;
}

export async function sha256(bytes: Uint8Array): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes as Uint8Array<ArrayBuffer>));
  return Array.from(d, (x) => x.toString(16).padStart(2, "0")).join("");
}

export interface OverviewOut {
  shift: number;
  first: number;
  buckets: number;
  channels: OverviewChannel[];
}

/**
 * An overview block's bytes for hashing, identical to `canonical_overview()` in bench/native:
 *   u8 shift, u64 first bucket, u64 buckets
 *   per channel, digital: u8 0, the level bytes, the change bytes
 *                analog:  u8 1, the minimums' then the maximums' f32 bits
 */
export function canonicalOverview(o: OverviewOut): Uint8Array {
  let len = 17;
  for (const c of o.channels) len += 1 + (c.kind === "digital" ? c.level.length + c.change.length : 4 * (c.min.length + c.max.length));
  const out = new Uint8Array(len);
  const v = new DataView(out.buffer);
  out[0] = o.shift;
  put64(v, 1, o.first);
  put64(v, 9, o.buckets);
  let at = 17;
  for (const c of o.channels) {
    out[at++] = c.kind === "digital" ? 0 : 1;
    const parts = c.kind === "digital" ? [c.level, c.change] : [c.min, c.max];
    for (const a of parts) {
      out.set(new Uint8Array(a.buffer, a.byteOffset, a.byteLength), at);
      at += a.byteLength;
    }
  }
  return out;
}
