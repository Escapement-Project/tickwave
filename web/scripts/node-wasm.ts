// Opens a .tickwave file in Node through the same WASM decoder and code the browser's worker uses.

import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { initSync, footerStart, Recording } from "../../crates/wasm/pkg/escapement_tickwave_wasm.js";
import { MemoryBytes, type Bytes } from "../src/wasm/bytes.ts";
import { Opened, type WasmApi } from "../src/wasm/opened.ts";

let api: WasmApi | null = null;

export function wasm(): WasmApi {
  if (!api) {
    initSync({ module: readFileSync(new URL("../../crates/wasm/pkg/escapement_tickwave_wasm_bg.wasm", import.meta.url)) });
    api = { footerStart, Recording };
  }
  return api;
}

export function openTickwave(path: string, bytes?: Bytes, firstRead?: number): Promise<Opened> {
  return Opened.open(wasm(), bytes ?? new MemoryBytes(readFileSync(path), basename(path)), firstRead);
}
