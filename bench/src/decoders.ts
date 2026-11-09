// The decoders under test, behind one interface the worker drives.

import { createXXHash64 } from "hash-wasm";
import { XXH64 } from "@intrnl/xxhash64";
import type { OverviewOut } from "./canonical.ts";
import { Head, headLength, FormatError, type Block, type BlockChannel, type OverviewChannel } from "./format.ts";
import { fzstdInflate, streamInflate, verify, type Xxh64Low } from "./zstd.ts";

/** Milliseconds spent in each step of the last `decode`. */
export type Phases = Record<string, number>;

export interface Opened {
  blockCount: number;
  blockRange(i: number): [number, number];
  decode(i: number, frame: Uint8Array): Block | Promise<Block>;
  /** Filled by `decode` when `timePhases` is set. */
  phases: Phases;
  /** Overview blocks, by level in the header's order then in time order; null without the API. */
  overviewCount: number | null;
  overviewRange(j: number): [number, number];
  decodeOverview(j: number, frame: Uint8Array): OverviewOut | Promise<OverviewOut>;
}

export interface Decoder {
  init(): Promise<void>;
  headLength(b: Uint8Array): number;
  open(head: Uint8Array, timePhases: boolean): Promise<Opened>;
}

// The wasm-bindgen glue's surface, the same for tickwave-wasm and the libzstd build.
interface WasmBlock {
  start: number;
  end: number;
  initial(c: number): boolean | undefined;
  edges(c: number): Float64Array | undefined;
  values(c: number): Float32Array | undefined;
  firstIndex(c: number): number | undefined;
  free(): void;
}
interface WasmOverviewBlock {
  first: number;
  buckets: number;
  digital(c: number): Uint8Array[] | undefined;
  analog(c: number): Float32Array[] | undefined;
  free(): void;
}
export interface Glue {
  default(init: { module_or_path: URL | string }): Promise<unknown>;
  headLength(b: Uint8Array): number;
  Recording: new (head: Uint8Array) => {
    header: string;
    blockCount: number;
    blockRange(i: number): Float64Array;
    decodeBlock(i: number, frame: Uint8Array): WasmBlock;
    inflateBlock?(i: number, frame: Uint8Array): number;
    parseBlock?(i: number, frame: Uint8Array): WasmBlock;
    overviewBlockRange?(level: number, k: number): Float64Array;
    decodeOverviewBlock?(level: number, k: number, frame: Uint8Array): WasmOverviewBlock;
  };
}

/**
 * Copies per block: the frame into WASM memory (wasm-bindgen's `&[u8]`), and each channel's
 * edges out of it (`Float64Array::from`), after an i64-to-f64 pass inside it.
 *
 * With `probe`, a block decodes as `inflateBlock` then `parseBlock` (bench/libzstd's probe
 * feature), timed apart. Each call copies the frame in, so a probe block has one copy more than
 * `decodeBlock`, inside the inflate step.
 */
export function wasmDecoder(glue: Glue, wasm: URL, probe = false): Decoder {
  return {
    async init() {
      await glue.default({ module_or_path: wasm });
    },
    headLength: (b) => glue.headLength(b),
    async open(head, timePhases) {
      const rec = new glue.Recording(head);
      const header = JSON.parse(rec.header);
      const kinds: string[] = header.channels.map((c: { kind: string }) => c.kind);
      const phases: Phases = {};
      // Overview block j as (level, k, shift), in the order the index holds them.
      const where: [number, number, number][] = [];
      (header.overview_blocks ?? []).forEach((l: { shift: number; blocks: number }, level: number) => {
        for (let k = 0; k < l.blocks; k++) where.push([level, k, l.shift]);
      });
      return {
        overviewCount: rec.decodeOverviewBlock ? where.length : null,
        overviewRange(j) {
          const r = rec.overviewBlockRange!(where[j][0], where[j][1]);
          return [r[0], r[1]];
        },
        decodeOverview(j, frame) {
          const [level, k, shift] = where[j];
          const o = rec.decodeOverviewBlock!(level, k, frame);
          try {
            const channels: OverviewChannel[] = kinds.map((kind, c) => {
              if (kind === "digital") {
                const [lv, ch] = o.digital(c)!;
                return { kind: "digital", level: lv, change: ch };
              }
              const [min, max] = o.analog(c)!;
              return { kind: "analog", min, max };
            });
            return { shift, first: o.first, buckets: o.buckets, channels };
          } finally {
            o.free();
          }
        },
        blockCount: rec.blockCount,
        blockRange(i) {
          const r = rec.blockRange(i);
          return [r[0], r[1]];
        },
        phases,
        decode(i, frame) {
          const t0 = timePhases ? performance.now() : 0;
          let b: WasmBlock;
          if (probe) {
            rec.inflateBlock!(i, frame);
            const tInflated = performance.now();
            b = rec.parseBlock!(i, frame);
            if (timePhases) {
              phases.inflate = tInflated - t0;
              phases.parse = performance.now() - tInflated;
            }
          } else b = rec.decodeBlock(i, frame);
          const t1 = timePhases ? performance.now() : 0;
          try {
            const channels: BlockChannel[] = kinds.map((k, c) =>
              k === "digital"
                ? { kind: "digital", initial: b.initial(c)!, edges: b.edges(c)! }
                : { kind: "analog", firstIndex: b.firstIndex(c)!, values: b.values(c)! },
            );
            return { start: b.start, end: b.end, channels };
          } finally {
            b.free();
            if (timePhases) {
              if (!probe) phases.decode = t1 - t0;
              phases.copyOut = performance.now() - t1;
            }
          }
        },
      };
    },
  };
}

export async function hashWasmXxh(): Promise<Xxh64Low> {
  const h = await createXXHash64(0, 0);
  return (d) => {
    h.init();
    h.update(d);
    const b = h.digest("binary") as Uint8Array;
    return ((b[4] << 24) | (b[5] << 16) | (b[6] << 8) | b[7]) >>> 0;
  };
}

// Its .d.ts says ArrayBuffer, but update() indexes bytes and reads .length: it takes a Uint8Array.
export const pureJsXxh: Xxh64Low = (d) =>
  Number(new XXH64(0).update(d as unknown as ArrayBuffer).digest() & 0xffffffffn);

/**
 * fzstd and the TypeScript reader. fzstd does not check the content checksum, so `xxh` does,
 * timed as its own phase. Copies per block: none into or out of WASM; fzstd allocates the
 * payload, the reader writes edges straight into the Float64Array it returns. With hash-wasm's
 * XXH64 the payload is copied once more, into that module's memory.
 */
export function fzstdDecoder(makeXxh: () => Promise<Xxh64Low>): Decoder {
  let xxh: Xxh64Low;
  const inflate = (f: Uint8Array, n: number) => {
    const out = fzstdInflate(f, n);
    verify(f, out, xxh);
    return out;
  };
  return {
    async init() {
      xxh = await makeXxh();
    },
    headLength,
    async open(bytes, timePhases) {
      const head = new Head(bytes);
      head.attachOverview(inflate(head.overviewFrame, head.overviewLen));
      const phases: Phases = {};
      return {
        blockCount: head.blockCount,
        blockRange: (i) => head.blockRange(i),
        phases,
        overviewCount: head.overviewRows.length,
        overviewRange: (j) => head.overviewBlockRange(j),
        decodeOverview(j, frame) {
          const row = head.overviewRows[j];
          if (frame.length !== row.compressed)
            throw new FormatError(`overview block ${j} is ${frame.length} bytes, the index says ${row.compressed}`);
          return head.decodeOverviewPayload(j, inflate(frame, row.decoded));
        },
        decode(i, frame) {
          const row = head.rows[i];
          if (frame.length !== row.compressed)
            throw new FormatError(`block ${i} is ${frame.length} bytes, the index says ${row.compressed}`);
          if (!timePhases) return head.decodePayload(i, inflate(frame, row.decoded));
          const t0 = performance.now();
          const out = fzstdInflate(frame, row.decoded);
          const t1 = performance.now();
          verify(frame, out, xxh);
          const t2 = performance.now();
          const block = head.decodePayload(i, out);
          phases.inflate = t1 - t0;
          phases.checksum = t2 - t1;
          phases.parse = performance.now() - t2;
          return block;
        },
      };
    },
  };
}

/**
 * `DecompressionStream("zstd")` and the TypeScript reader. The stream's output is copied once
 * into a payload buffer of the index's length.
 */
export function streamDecoder(): Decoder {
  return {
    async init() {},
    headLength,
    async open(bytes, timePhases) {
      const head = new Head(bytes);
      head.attachOverview(await streamInflate(head.overviewFrame, head.overviewLen));
      const phases: Phases = {};
      return {
        blockCount: head.blockCount,
        blockRange: (i) => head.blockRange(i),
        phases,
        overviewCount: head.overviewRows.length,
        overviewRange: (j) => head.overviewBlockRange(j),
        async decodeOverview(j, frame) {
          const row = head.overviewRows[j];
          if (frame.length !== row.compressed)
            throw new FormatError(`overview block ${j} is ${frame.length} bytes, the index says ${row.compressed}`);
          return head.decodeOverviewPayload(j, await streamInflate(frame, row.decoded));
        },
        async decode(i, frame) {
          const row = head.rows[i];
          if (frame.length !== row.compressed)
            throw new FormatError(`block ${i} is ${frame.length} bytes, the index says ${row.compressed}`);
          const t0 = timePhases ? performance.now() : 0;
          const out = await streamInflate(frame, row.decoded);
          const t1 = timePhases ? performance.now() : 0;
          const block = head.decodePayload(i, out);
          if (timePhases) {
            phases.inflate = t1 - t0;
            phases.parse = performance.now() - t1;
          }
          return block;
        },
      };
    },
  };
}
