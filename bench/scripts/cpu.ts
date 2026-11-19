// The machine's CPU busy fraction, sampled each second from /proc/stat. Other builds may share the
// machine, so each timed unit waits for a quiet second first and records how busy the machine was
// while it ran.
//   node scripts/cpu.ts      waits for a load average below 2 for five minutes, then for three quiet
//                            seconds, then exits: a gate for other commands

import { readFileSync } from "node:fs";
import { loadavg } from "node:os";

function times(): { idle: number; total: number } {
  const f = readFileSync("/proc/stat", "utf8").split("\n")[0].trim().split(/\s+/).slice(1).map(Number);
  return { idle: f[3] + f[4], total: f.reduce((a, b) => a + b, 0) };
}

/** `[ms since the epoch, busy fraction of every CPU over the second before]` */
export const samples: [number, number][] = [];
let prev = times();
const timer = setInterval(() => {
  const c = times();
  samples.push([Date.now(), 1 - (c.idle - prev.idle) / (c.total - prev.total)]);
  prev = c;
}, 1000);
timer.unref();

/**
 * Mean busy fraction over the samples taken in `[from, to]`, and the second after. Called as a unit
 * ends, it has no sample yet for a unit under a second; scripts/report.ts recomputes it from
 * `cpu.json` with the unit's `at` and `end`.
 */
export function busyBetween(from: number, to: number): number {
  const xs = samples.filter(([t]) => t > from && t <= to + 1000).map(([, x]) => x);
  return xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : NaN;
}

/**
 * Resolves once three seconds in a row have a busy fraction below `limit`. By default it waits as
 * long as that takes, saying so every 10 minutes; a finite `maxMs` gives up after that long and
 * says so.
 */
export async function quiet(limit = 0.1, maxMs = Infinity, say: (s: string) => void = console.error): Promise<number> {
  const start = Date.now();
  let told = start;
  for (;;) {
    const last = samples.at(-1);
    const recent = samples.slice(-3);
    if (last && Date.now() - last[0] < 1500 && recent.length === 3 && recent.every(([, x]) => x < limit))
      return Date.now() - start;
    if (Date.now() - start > maxMs) {
      say(`machine still busy (${last ? (100 * last[1]).toFixed(0) : "?"} %) after ${maxMs / 1000} s; going on`);
      return Date.now() - start;
    }
    if (Date.now() - told > 600_000) {
      told = Date.now();
      say(`waiting for a quiet machine for ${((told - start) / 60_000).toFixed(0)} min`);
    }
    await new Promise((r) => setTimeout(r, 250));
  }
}

/**
 * Resolves once the 1-minute load average (as `uptime` shows it) has stayed below `limit` for
 * `holdMs`, checked every 10 s. Waits as long as that takes, saying so every 10 minutes.
 */
export async function settled(limit = 2, holdMs = 300_000, say: (s: string) => void = console.error): Promise<number[]> {
  const start = Date.now();
  let since = Date.now();
  let told = start;
  for (;;) {
    if (loadavg()[0] >= limit) since = Date.now();
    else if (Date.now() - since >= holdMs) return loadavg();
    if (Date.now() - told > 600_000) {
      told = Date.now();
      say(`waiting for the load average to stay below ${limit}: ${loadavg()[0].toFixed(2)} now, ${((told - start) / 60_000).toFixed(0)} min so far`);
    }
    await new Promise((r) => setTimeout(r, 10_000));
  }
}

if (import.meta.main) {
  const load = await settled();
  const waited = await quiet();
  console.error(`load average ${load.map((x) => x.toFixed(2)).join(" ")}; quiet after ${(waited / 1000).toFixed(0)} s more`);
  process.exit(0);
}
