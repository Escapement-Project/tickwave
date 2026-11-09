// zstd inflates for the JavaScript decoders. FORMAT.md: every frame carries a content checksum; a
// reader refuses a frame without one, or with one that does not match. The checksum is the low 32
// bits of XXH64 (seed 0) over the decoded content, stored little-endian in the frame's last 4 bytes.

import { decompress } from "fzstd";
import { FormatError } from "./format.ts";

/** Low 32 bits of XXH64 with seed 0. */
export type Xxh64Low = (data: Uint8Array) => number;

/**
 * Refuses anything but exactly one zstd frame that declares a content checksum and no dictionary.
 * Both fzstd and DecompressionStream decode every frame in their input, so the frame's end is found
 * here, by walking its block headers, and must be the input's end.
 */
export function checkFrame(f: Uint8Array): void {
  if (f.length < 9 || f[0] !== 0x28 || f[1] !== 0xb5 || f[2] !== 0x2f || f[3] !== 0xfd)
    throw new FormatError("not a zstd frame");
  const fhd = f[4];
  if (fhd & 0x08) throw new FormatError("zstd frame header's reserved bit is set");
  if (!(fhd & 0x04)) throw new FormatError("zstd frame has no content checksum");
  if (fhd & 0x03) throw new FormatError("zstd frame names a dictionary");
  const single = (fhd >> 5) & 1;
  const fcf = fhd >> 6;
  let at = 5 + (single ? 0 : 1) + [0, 1, 2, 4][fhd & 3] + (fcf === 0 ? single : 1 << fcf);
  for (;;) {
    if (at + 3 > f.length) throw new FormatError("zstd frame is cut short");
    const h = f[at] | (f[at + 1] << 8) | (f[at + 2] << 16);
    const type = (h >> 1) & 3;
    if (type === 3) throw new FormatError("zstd block of the reserved type");
    at += 3 + (type === 1 ? 1 : h >>> 3);
    if (h & 1) break;
  }
  if (fhd & 0x04) at += 4;
  if (at !== f.length) throw new FormatError(`zstd frame is ${at} bytes, followed by ${f.length - at} more`);
}

export function storedChecksum(f: Uint8Array): number {
  const n = f.length;
  return (f[n - 4] | (f[n - 3] << 8) | (f[n - 2] << 16) | (f[n - 1] << 24)) >>> 0;
}

/** fzstd steps over the checksum unread; `verify` checks it. */
export function fzstdInflate(frame: Uint8Array, len: number): Uint8Array {
  checkFrame(frame);
  const out = decompress(frame);
  if (out.length !== len) throw new FormatError(`zstd frame decoded to ${out.length} bytes, not ${len}`);
  return out;
}

export function verify(frame: Uint8Array, out: Uint8Array, xxh: Xxh64Low): void {
  const want = storedChecksum(frame);
  const got = xxh(out);
  if (got !== want)
    throw new FormatError(`zstd content checksum ${got.toString(16)} does not match ${want.toString(16)}`);
}

/**
 * `DecompressionStream("zstd")`, where the browser has it. Whether the browser checks the content
 * checksum is a property of its implementation; the corruption test finds out per browser.
 */
export async function streamInflate(frame: Uint8Array, len: number): Promise<Uint8Array> {
  checkFrame(frame);
  const ds = new DecompressionStream("zstd" as CompressionFormat);
  const writer = ds.writable.getWriter();
  const written = writer.write(frame as Uint8Array<ArrayBuffer>).then(() => writer.close());
  const reader = ds.readable.getReader();
  const out = new Uint8Array(len);
  let n = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (n + value.length > len) throw new FormatError(`zstd frame decodes past ${len} bytes`);
      out.set(value, n);
      n += value.length;
    }
    await written;
  } catch (e) {
    written.catch(() => {});
    reader.cancel().catch(() => {});
    throw e;
  }
  if (n !== len) throw new FormatError(`zstd frame decoded to ${n} bytes, not ${len}`);
  return out;
}

export function hasStreamZstd(): boolean {
  try {
    new DecompressionStream("zstd" as CompressionFormat);
    return true;
  } catch {
    return false;
  }
}
