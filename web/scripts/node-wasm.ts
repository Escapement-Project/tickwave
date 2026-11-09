// Opens a .chrono file in Node through the same WASM decoder and code the browser's worker uses.

import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { initSync, headLength, Recording } from "../../crates/wasm/pkg/chrono_wasm.js";
import { MemoryBytes, type Bytes } from "../src/wasm/bytes.ts";
import { Opened, type WasmApi } from "../src/wasm/opened.ts";

let api: WasmApi | null = null;

export function wasm(): WasmApi {
  if (!api) {
    initSync({ module: readFileSync(new URL("../../crates/wasm/pkg/chrono_wasm_bg.wasm", import.meta.url)) });
    api = { headLength, Recording };
  }
  return api;
}

export function openChrono(path: string, bytes?: Bytes, firstRead?: number): Promise<Opened> {
  return Opened.open(wasm(), bytes ?? new MemoryBytes(readFileSync(path), basename(path)), firstRead);
}
