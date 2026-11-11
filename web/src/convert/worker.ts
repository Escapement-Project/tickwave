// The converter's worker: the WASM converter, reading the files the page posts a slice at a time.
// It does one job, describing or converting, and the page then ends it: a read that fails throws
// through the module, which is not fit to run again after that.

import init, { convert, describe } from "../../../crates/convert-wasm/pkg/escapement_tickwave_convert_wasm.js";
import { request, type FromWorker, type Pass, type ToWorker } from "./protocol.ts";

// A worker's own globals, which the page's DOM types leave out.
declare const FileReaderSync: { new (): { readAsArrayBuffer(blob: Blob): ArrayBuffer } };
const scope = self as unknown as {
  postMessage(m: FromWorker, transfer: Transferable[]): void;
  onmessage: ((e: MessageEvent<ToWorker>) => void) | null;
};

const post = (m: FromWorker, transfer: Transferable[] = []) => scope.postMessage(m, transfer);

let files: File[] = [];
let panic: string | null = null;
const reader = new FileReaderSync();

(globalThis as Record<string, unknown>).tickwaveConvert = {
  read(file: number, offset: number, into: Uint8Array): void {
    const f = files[file];
    let bytes: ArrayBuffer;
    try {
      bytes = reader.readAsArrayBuffer(f.slice(offset, offset + into.length));
    } catch (e) {
      throw new Error(`${f.name} could not be read: it may have changed or moved since it was chosen (${e instanceof Error ? e.message : e})`);
    }
    if (bytes.byteLength !== into.length) throw new Error(`${f.name} ended early: it may have changed since it was chosen`);
    into.set(new Uint8Array(bytes));
  },
  start(pass: Pass, channel: string, total: number): void {
    post({ op: "start", pass, channel, total });
  },
  step(n: number): void {
    post({ op: "step", n });
  },
  panicked(message: string): void {
    panic = message;
  },
};

scope.onmessage = async (e) => {
  const m = e.data;
  try {
    await init({ module_or_path: new URL("./tickwave-convert.wasm", import.meta.url) });
    files = m.files;
    const refs = files.map((f, id) => ({ id, name: f.name, size: f.size }));
    if (m.op === "describe") {
      post({ op: "described", described: JSON.parse(describe(request(m.kind, refs))) });
      return;
    }
    const started = performance.now();
    const out = convert(request(m.kind, refs, m.settings));
    const file = out.file;
    const codes = JSON.parse(out.codes) as [string, number | null][];
    out.free();
    const buffer = file.buffer as ArrayBuffer;
    post({ op: "converted", file: buffer, codes, seconds: (performance.now() - started) / 1000 }, [buffer]);
  } catch (err) {
    post({ op: "failed", message: panic ?? (err instanceof Error ? err.message : String(err)) });
  }
};
