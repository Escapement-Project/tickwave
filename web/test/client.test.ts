// The page's side of the worker protocol, against a stand-in worker that answers each open.

import { test } from "node:test";
import assert from "node:assert/strict";

type Message = { op: string; id?: number; channel?: number; requests?: Message[] };
const posted: Message[] = [];
class StandIn {
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onerror: unknown = null;
  postMessage(m: Message): void {
    posted.push(m);
    if (m.op === "open") setTimeout(() => this.onmessage?.({ data: { id: m.id, ok: true, value: { length: 10, channels: [] } } }));
  }
}
(globalThis as { Worker?: unknown }).Worker = StandIn;
// A page of the module's own origin, so the worker starts from its own URL.
(globalThis as { location?: unknown }).location = { origin: new URL(import.meta.url).origin };
const { WasmSource } = await import("../src/wasm/client.ts");

/** Each message posted since `from`, as its requests' op and channel. */
const messages = (from: number) => posted.slice(from).map((m) => (m.op === "batch" ? m.requests! : [m]).map((r) => `${r.op} ${r.channel}`));

test("the requests made before the first is posted reach the worker in one message", async () => {
  const s = new WasmSource("client-one", { url: "http://127.0.0.1/a.tickwave" });
  await s.info();
  const from = posted.length;
  for (const c of [0, 1, 2]) void s.edges(c, 0, 10);
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(messages(from), [["edges 0", "edges 1", "edges 2"]]);
});

test("a request aborted before its message goes is never posted, and asks nothing to stop", async () => {
  const s = new WasmSource("client-abort", { url: "http://127.0.0.1/b.tickwave" });
  await s.info();
  const from = posted.length;
  const a = new AbortController();
  const gone = s.edges(0, 0, 10, a.signal);
  void s.edges(1, 0, 10);
  // After both have been made into requests, before they are posted.
  queueMicrotask(() => a.abort());
  await assert.rejects(gone, { name: "AbortError" });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(messages(from), [["edges 1"]]);
});
