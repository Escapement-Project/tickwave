// The cold first-inset page: `inset.html?dec=<decoder>&rec=<url>[&block=<i>]`. It starts the
// decoder's worker at once, which fetches and compiles the decoder while it fetches the head,
// then fetches one block and decodes it; the page draws it. `window.__result` holds the times,
// in ms from navigation start.

import { drawBlock } from "./draw.ts";

declare global {
  interface Window {
    __result?: unknown;
  }
}

const scriptStart = performance.now();
const q = new URLSearchParams(location.search);
const rel = (abs: number) => abs - performance.timeOrigin;
const worker = new Worker(`dec/${q.get("dec")}/worker.js`, { type: "module" });
worker.postMessage({ id: 1, op: "inset", url: q.get("rec"), block: q.has("block") ? Number(q.get("block")) : undefined });
let workerReady = NaN;
worker.onmessage = (ev) => {
  const m = ev.data;
  if (m.id === "ready") {
    workerReady = rel(m.t);
    return;
  }
  if (!m.ok) {
    window.__result = { error: m.error };
    return;
  }
  const received = performance.now();
  const canvas = document.querySelector("canvas")!;
  drawBlock(canvas, m.block);
  // Forces the raster now rather than at the next frame.
  canvas.getContext("2d")!.getImageData(0, 0, 1, 1);
  const drawn = performance.now();
  const phases: Record<string, number> = {};
  for (const [k, v] of Object.entries(m.t as Record<string, number>)) phases[k] = rel(v);
  window.__result = {
    drawn,
    received,
    scriptStart,
    workerReady,
    phases,
    block: m.i,
    headBytes: m.headBytes,
    blockBytes: m.blockBytes,
    crossOriginIsolated: self.crossOriginIsolated,
    resources: performance.getEntriesByType("resource").map((r) => {
      const e = r as PerformanceResourceTiming;
      return { name: e.name, transferSize: e.transferSize, encodedBodySize: e.encodedBodySize, decodedBodySize: e.decodedBodySize };
    }),
  };
};
