// Runs the browser's converter in Node over files on disk, through the same WASM module and
// requests the browser's worker uses.

import { closeSync, fstatSync, openSync, readFileSync, readSync } from "node:fs";
import { basename } from "node:path";
import { initSync, convert, describe } from "../../crates/convert-wasm/pkg/escapement_tickwave_convert_wasm.js";
import { DEFAULTS, request, type Described, type Pass, type Settings, type SourceKind } from "../src/convert/protocol.ts";

let memory: WebAssembly.Memory | null = null;

/** The module's memory, in bytes: its peak so far, since WASM memory never shrinks. */
export const memoryBytes = () => memory?.buffer.byteLength ?? 0;

export interface Hooks {
  start?: (pass: Pass, channel: string, total: number) => void;
  step?: (n: number) => void;
}

/** Opens `paths` for the module's reads while `run` runs. */
function withFiles<T>(paths: string[], hooks: Hooks, run: (files: { id: number; name: string; size: number }[]) => T): T {
  memory ??= (initSync({ module: readFileSync(new URL("../../crates/convert-wasm/pkg/escapement_tickwave_convert_wasm_bg.wasm", import.meta.url)) }) as unknown as { memory: WebAssembly.Memory }).memory;
  const fds = paths.map((p) => openSync(p, "r"));
  let panic: string | null = null;
  (globalThis as Record<string, unknown>).tickwaveConvert = {
    read(file: number, offset: number, into: Uint8Array) {
      for (let done = 0; done < into.length; ) {
        const n = readSync(fds[file], into, done, into.length - done, offset + done);
        if (n === 0) throw new Error(`${paths[file]}: ended early`);
        done += n;
      }
    },
    start: hooks.start ?? (() => {}),
    step: hooks.step ?? (() => {}),
    panicked(message: string) {
      panic = message;
    },
  };
  try {
    return run(paths.map((p, id) => ({ id, name: basename(p), size: fstatSync(fds[id]).size })));
  } catch (e) {
    throw panic ? new Error(panic) : e;
  } finally {
    for (const fd of fds) closeSync(fd);
  }
}

export function describeFiles(kind: SourceKind, paths: string[]): Described {
  return withFiles(paths, {}, (files) => JSON.parse(describe(request(kind, files))));
}

export function convertFiles(kind: SourceKind, paths: string[], settings: Partial<Settings>, hooks: Hooks = {}): { file: Uint8Array; codes: [string, number | null][] } {
  return withFiles(paths, hooks, (files) => {
    const out = convert(request(kind, files, { ...DEFAULTS, ...settings }));
    try {
      return { file: out.file, codes: JSON.parse(out.codes) };
    } finally {
      out.free();
    }
  });
}
