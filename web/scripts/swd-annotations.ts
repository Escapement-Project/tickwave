// Finds SWD line resets, the JTAG-to-SWD select sequence and every transfer in a fixture, and
// writes them as a generator's annotation file (PLAN.md §3, JSON Lines). Run from web/:
//
//   node scripts/swd-annotations.ts fixtures/identify-swd.chronofx > fixtures/identify-swd.swd.jsonl
//
// A long capture holds hundreds of thousands of transfers, more than a sidecar is for. There,
// --kinds wait,fault keeps only those kinds, and --until TICK keeps everything before TICK too.
//
// A bit is SWDIO's level just before an SWCLK rising edge. The probe changes SWDIO on the falling
// edge, the target a little after the rising edge, so a probe-driven bit's cell runs falling to
// falling around its rising edge, and a target-driven bit's runs from the rising edge before it.

import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { readFixture } from "../src/fixture-file.ts";
import type { MemoryDigital } from "../src/memory-source.ts";
import { lowerBound } from "../src/search.ts";

const { values: opts, positionals } = parseArgs({
  allowPositionals: true,
  options: { kinds: { type: "string" }, until: { type: "string" } },
});
const rec = await readFixture(readFileSync(positionals[0]));
const channel = (name: string) => {
  const c = rec.channels.find((c) => c.name === name);
  if (c?.kind !== "digital") throw new Error(`no digital ${name}`);
  return c as MemoryDigital;
};
const clk = channel("SWCLK");
const dio = channel("SWDIO");
const levelAfter = (c: MemoryDigital, i: number) => c.initial ^ ((i + 1) & 1);
const rising: number[] = [];
const falling: number[] = [];
clk.ticks.forEach((t, i) => (levelAfter(clk, i) ? rising : falling).push(t));
const bits = rising.map((r) => dio.initial ^ (lowerBound(dio.ticks, r) & 1));

const fallingBefore = (t: number) => falling[lowerBound(falling, t) - 1];
// SWCLK idles high between bursts, so the falling edge after a burst's last bit can be far off.
const cellEnd = (r: number) => Math.min(falling[lowerBound(falling, r + 1)] ?? Infinity, 2 * r - fallingBefore(r));
/** Bits [a, b) as the probe drives them. */
const probeSpan = (a: number, b: number) => [fallingBefore(rising[a]), cellEnd(rising[b - 1])];
/** Bits [a, b) as the target drives them. */
const targetSpan = (a: number, b: number) => [rising[a - 1], rising[b - 1]];

const out: object[] = [];
const note = (kind: string, [start, end]: number[], fields: object) => out.push({ id: `${kind}/${start}`, kind, start, end, ...fields });
const word = (a: number) => {
  let v = 0;
  for (let k = 0; k < 32; k++) v += bits[a + k] * 2 ** k;
  return v;
};
const hex = (v: number, digits: number) => `0x${v.toString(16).toUpperCase().padStart(digits, "0")}`;
const DP_READ = ["DPIDR", "CTRL/STAT", "RESEND", "RDBUFF"];
const DP_WRITE = ["ABORT", "CTRL/STAT", "SELECT", "TARGETSEL"];
const ACK: Record<number, string> = { 1: "OK", 2: "WAIT", 4: "FAULT" };

let i = 0;
while (i < bits.length) {
  let ones = 0;
  while (i + ones < bits.length && bits[i + ones] === 1) ones++;
  if (ones >= 50) {
    note("reset", probeSpan(i, i + ones), { channels: ["SWCLK", "SWDIO"], label: `line reset, ${ones} clocks` });
    i += ones;
    continue;
  }
  let seq = 0;
  for (let k = 0; k < 16 && i + k < bits.length; k++) seq += bits[i + k] << k;
  if (seq === 0xe79e) {
    note("select", probeSpan(i, i + 16), { channels: ["SWCLK", "SWDIO"], label: "JTAG to SWD" });
    i += 16;
    continue;
  }
  const req = bits.slice(i, i + 8);
  const parity = (req[1] + req[2] + req[3] + req[4]) % 2;
  if (req.length === 8 && req[0] === 1 && req[5] === parity && req[6] === 0 && req[7] === 1) {
    const ap = req[1] === 1;
    const read = req[2] === 1;
    const reg = req[3] + 2 * req[4];
    const ack = bits[i + 9] + 2 * bits[i + 10] + 4 * bits[i + 11];
    const name = ap ? `AP ${read ? "read" : "write"} ${hex(reg * 4, 1)}` : `${(read ? DP_READ : DP_WRITE)[reg]} ${read ? "read" : "write"}`;
    note("request", probeSpan(i, i + 8), { channels: ["SWDIO"], label: "request", paint: "probe" });
    const ackKind = ack === 1 ? "ack" : (ACK[ack] ?? "no-ack").toLowerCase();
    note(ackKind, targetSpan(i + 9, i + 12), { channels: ["SWDIO"], label: `ack ${ACK[ack] ?? hex(ack, 1)}`, paint: "target" });
    let last = i + 12;
    let data = "";
    if (ack === 1 && read) {
      note("data", targetSpan(i + 12, i + 45), { channels: ["SWDIO"], label: "data", paint: "target" });
      data = ` ${hex(word(i + 12), 8)}`;
      last = i + 46;
    } else if (ack === 1) {
      note("data", probeSpan(i + 13, i + 46), { channels: ["SWDIO"], label: "data", paint: "probe" });
      data = ` ${hex(word(i + 13), 8)}`;
      last = i + 46;
    }
    const [start] = probeSpan(i, i + 8);
    const end = cellEnd(rising[last - 1]);
    const label = `${name}${data}${ack === 1 ? "" : `, ${ACK[ack] ?? "no ack"}`}`;
    note(read ? "read" : "write", [start, end], { channels: ["SWCLK", "SWDIO"], label });
    i = last;
    continue;
  }
  i++;
}

type Note = { start: number; kind: string };
const kinds = opts.kinds ? new Set(opts.kinds.split(",")) : null;
const until = opts.until ? Number(opts.until) : -Infinity;
const kept = (out as Note[]).filter((a) => kinds === null || kinds.has(a.kind) || a.start < until);
kept.sort((a, b) => a.start - b.start);
console.log(JSON.stringify({ format: "chrono-annotations/1", source: "swd" }));
for (const a of kept) console.log(JSON.stringify(a));
