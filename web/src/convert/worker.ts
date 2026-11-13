// The converter's worker: the WASM converter, reading the files the page posts a slice at a time.
// It does one job, describing, converting or unzipping, and the page then ends it: a read that
// fails throws through the module, which is not fit to run again after that. A worker the page
// keeps for editing a sidecar's hand-written file takes only edits, which read no files.

import init, { convert, describe, editAnnotations, merge, unzip } from "../../../crates/convert-wasm/pkg/escapement_tickwave_convert_wasm.js";
import { handJson } from "../wasm/hand.ts";
import { request, type FromWorker, type HandAnnotation, type Pass, type ToWorker } from "./protocol.ts";

// A worker's own globals, which the page's DOM types leave out.
declare const FileReaderSync: { new (): { readAsArrayBuffer(blob: Blob): ArrayBuffer; readAsText(blob: Blob): string } };
const scope = self as unknown as {
  postMessage(m: FromWorker, transfer: Transferable[]): void;
  onmessage: ((e: MessageEvent<ToWorker>) => void) | null;
};

const post = (m: FromWorker, transfer: Transferable[] = []) => scope.postMessage(m, transfer);

let files: File[] = [];
let panic: string | null = null;
/** The bytes of the zip's deflated files as they unzip, one file after another. */
let chunks: Uint8Array<ArrayBuffer>[] = [];
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
  unzipped(chunk: Uint8Array): void {
    chunks.push(chunk.slice());
  },
};

scope.onmessage = async (e) => {
  const m = e.data;
  try {
    await init({ module_or_path: new URL("./tickwave-convert.wasm", import.meta.url) });
    if (m.op === "edit") {
      const text = m.edit ? editAnnotations(m.text, JSON.stringify(m.edit)) : m.text;
      const hand = (JSON.parse(handJson(text, "the TOML file").json) as { annotation?: HandAnnotation[] }).annotation ?? [];
      post({ op: "edited", text, hand });
      return;
    }
    if (m.op === "unzip") {
      post({ op: "unzipped", files: unzipped(m.file, m.ends) });
      return;
    }
    if (m.op === "merge") {
      files = m.inputs.map((i) => i.file);
      const inputs = m.inputs.map((i, id) => ({
        file: { id, name: i.file.name, size: i.file.size },
        stem: i.stem,
        align: i.align,
        channels: i.channels,
        ...(i.sidecar.length > 0 && { annotations: Object.fromEntries(i.sidecar.map((f) => [f.name, reader.readAsText(f)])) }),
      }));
      const started = performance.now();
      const out = merge(JSON.stringify({ inputs, ...m.settings }));
      const file = out.file;
      const report = JSON.parse(out.report) as string[];
      out.free();
      const buffer = file.buffer as ArrayBuffer;
      post({ op: "merged", file: buffer, report, seconds: (performance.now() - started) / 1000 }, [buffer]);
      return;
    }
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

/** The files in `zip` the page opens: a stored one sliced from the zip, a deflated one from its
 * chunks, which come in the files' order. */
function unzipped(zip: File, ends: string[]): File[] {
  files = [zip];
  chunks = [];
  const listed = JSON.parse(unzip(JSON.stringify({ file: { id: 0, name: zip.name, size: zip.size }, ends }))) as { name: string; size: number; start: number | null }[];
  let next = 0;
  return listed.map((f) => {
    if (f.start !== null) return new File([zip.slice(f.start, f.start + f.size)], f.name);
    const parts: Uint8Array<ArrayBuffer>[] = [];
    let got = 0;
    while (got < f.size && next < chunks.length) {
      parts.push(chunks[next]);
      got += chunks[next++].length;
    }
    if (got !== f.size) throw new Error(`${f.name} unzipped to ${got} bytes, not the ${f.size} the zip states`);
    return new File(parts, f.name);
  });
}
