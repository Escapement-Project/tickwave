// Runs the benchmark's measures in Chromium and Firefox and writes the raw results to
// results/raw/. scripts/report.ts turns them into tables.
//   node scripts/run.ts [--measures correct,inset,blocks,whole,checksum] [--browsers chromium,firefox]
//                       [--runs 20] [--recs a,b] [--out results/raw]

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { cpus, loadavg } from "node:os";
import { parseArgs } from "node:util";
import cliProgress from "cli-progress";
import { chromium, firefox, type Browser, type Page } from "playwright";
import { busyBetween, quiet, samples, settled } from "./cpu.ts";
import { CONVERTED, start } from "./serve.ts";

const RECS = [
  "gcc-swd-reads.1024",
  "stock-reads.1024",
  "swo-24000000.1024",
  "gcc-oscan1-probe-rs.1024",
  "gcc-swd-matrix-capture100000.1024",
  "session50.1024",
];
const LARGEST = "gcc-swd-matrix-capture100000.1024";

// DevTools' "Slow 4G" (devtools-frontend front_end/core/sdk/NetworkManager.ts, Slow4GConditions).
export const SLOW_4G = {
  offline: false,
  latency: 150 * 3.75,
  downloadThroughput: ((1.6 * 1000 * 1000) / 8) * 0.9,
  uploadThroughput: ((750 * 1000) / 8) * 0.9,
};

// Firefox 159 has DecompressionStream("zstd") behind this pref, off by default.
const FIREFOX_PREFS = { "dom.compression_streams.zstd.enabled": true };

// `<dir>#2` is the A-A twin: the same worker bundle, run as a second decoder.
const INSET = ["ruzstd-full", "ruzstd", "ruzstd#2", "libzstd", "fzstd", "stream"];
const BLOCKS = ["ruzstd", "ruzstd#2", "libzstd", "fzstd", "stream"];
const CORRECT = ["ruzstd-full", "ruzstd", "libzstd", "fzstd", "fzstd-jsxxh", "stream"];
const CHECKSUM = ["fzstd", "fzstd-jsxxh"];
const dir = (label: string) => label.split("#")[0];
/** A measure's decoders, or the ones `--decoders` names instead. */
const pick = (labels: string[]) => (args.decoders ? args.decoders.split(",") : labels);

const { values: args } = parseArgs({
  options: {
    measures: { type: "string", default: "correct,inset,blocks,whole,checksum" },
    browsers: { type: "string", default: "chromium,firefox" },
    runs: { type: "string", default: "20" },
    recs: { type: "string", default: RECS.join(",") },
    out: { type: "string", default: "results/raw" },
    decoders: { type: "string" },
  },
});
const measures = args.measures!.split(",");
const runs = Number(args.runs);
const recs = args.recs!.split(",");
mkdirSync(args.out!, { recursive: true });

const bars = new cliProgress.MultiBar(
  { format: "{task} [{bar}] {value}/{total} eta {eta_formatted}", noTTYOutput: true, notTTYSchedule: 30000, hideCursor: true },
  cliProgress.Presets.shades_classic,
);
const log = (s: string) => bars.log(s + "\n");

const server = await start();
const types = { chromium, firefox } as const;
type Name = keyof typeof types;

function launch(name: Name, prefs = true): Promise<Browser> {
  return name === "firefox" ? firefox.launch({ firefoxUserPrefs: prefs ? FIREFOX_PREFS : {} }) : chromium.launch();
}

// Other agents build in this tree, so a measure may share the CPU; the load average says when.
const loadAtStart = new Map<string, number[]>();
const mark = (name: string) => loadAtStart.set(name, loadavg());
function save(name: string, data: any) {
  if (!loadAtStart.has(name)) loadAtStart.set(name, loadavg());
  writeFileSync(`${args.out}/${name}.json`, JSON.stringify({ ...data, load: { start: loadAtStart.get(name), end: loadavg() } }));
}

/** Rotates the order each run, so no decoder always runs first or last. */
function rotate<T>(xs: T[], r: number): T[] {
  return xs.map((_, i) => xs[(i + r) % xs.length]);
}

async function benchPage(browser: Browser): Promise<Page> {
  const page = await (await browser.newContext()).newPage();
  page.on("pageerror", (e) => log(`pageerror: ${e.message}`));
  await page.goto(`${server.url}/bench.html`);
  await page.waitForFunction(() => (globalThis as any).bench);
  return page;
}

type Features = { stream: boolean; crossOriginIsolated: boolean; userAgent: string };
const features = (page: Page) => page.evaluate(() => (globalThis as any).bench.features()) as Promise<Features>;

async function available(page: Page, labels: string[]): Promise<string[]> {
  const f = await features(page);
  return labels.filter((l) => dir(l) !== "stream" || f.stream);
}

async function setup(page: Page, labels: string[], rec: string): Promise<number> {
  await page.evaluate((d) => (globalThis as any).bench.setup(d), Object.fromEntries(labels.map((l) => [l, dir(l)])));
  return page.evaluate((u) => (globalThis as any).bench.load(u), `/rec/${rec}.chrono`);
}

async function correct(name: Name, browser: Browser) {
  mark(`correct.${name}`);
  const out: any = { browser: name, version: browser.version() };
  const plain = await launch(name, false);
  out.defaultFeatures = await features(await benchPage(plain));
  await plain.close();
  const page = await benchPage(browser);
  out.features = await features(page);
  const labels = await available(page, pick(CORRECT));
  out.hashes = {};
  out.overview = {};
  const list = ["identify-swd", ...recs];
  const bar = bars.create(list.length * labels.length, 0, { task: `${name} hashes` });
  for (const rec of list) {
    const ref = JSON.parse(readFileSync(`build/reference/${rec}.json`, "utf8"));
    await setup(page, labels, rec);
    for (const l of labels) {
      const got: string[] = await page.evaluate((x) => (globalThis as any).bench.hash(x), l);
      const bad = ref.blocks.map((b: any, i: number) => (b.hash === got[i] ? -1 : i)).filter((i: number) => i >= 0);
      if (got.length !== ref.blocks.length) bad.push(`count ${got.length} != ${ref.blocks.length}`);
      out.hashes[`${rec} ${l}`] = { blocks: got.length, mismatched: bad };
      if (bad.length) log(`${name} ${rec} ${l}: ${bad.length} blocks differ from the native decoder`);
      const ov: string[] | null = await page.evaluate((x) => (globalThis as any).bench.hashOverview(x), l);
      if (ov === null) {
        out.overview[`${rec} ${l}`] = null;
      } else {
        const badOv = ref.overview.map((h: string, j: number) => (h === ov[j] ? -1 : j)).filter((j: number) => j >= 0);
        if (ov.length !== ref.overview.length) badOv.push(`count ${ov.length} != ${ref.overview.length}`);
        out.overview[`${rec} ${l}`] = { blocks: ov.length, mismatched: badOv };
        if (badOv.length) log(`${name} ${rec} ${l}: ${badOv.length} overview blocks differ from the native decoder`);
      }
      bar.increment();
    }
  }
  out.corrupt = [];
  for (const file of ["gcc-swd-reads.1024", "session50.1024"]) {
    const m = JSON.parse(readFileSync(`build/corrupt/${file}.corrupt.json`, "utf8"));
    await setup(page, labels, "identify-swd");
    for (const c of m.corruptions) {
      for (const l of labels) {
        const url = c.whole_file ? `/corrupt/${c.path}` : `/rec/${file}.chrono`;
        const frame = c.whole_file ? null : `/corrupt/${c.path}`;
        const block = c.block ?? m.block;
        const expected = c.stage ?? "block";
        const r = await page.evaluate(([x, u, b, f]) => (globalThis as any).bench.corrupt(x, u, b, f), [l, url, block, frame] as const);
        out.corrupt.push({ file, kind: c.kind, decoder: l, native: c.native, expected, ...r });
        if (!r.refused) log(`${name} ${file} ${c.kind}: ${l} ACCEPTED a corrupted block`);
        else if (r.stage !== expected) log(`${name} ${file} ${c.kind}: ${l} refused at ${r.stage}, not ${expected}`);
      }
    }
  }
  bar.stop();
  await page.context().close();
  save(`correct.${name}`, out);
}

// A fresh browser a run: Firefox compiles the first WASM module in a process about 50 ms slower
// than later ones (scripts/cold-check.ts), so a new context in a running browser is not cold.
async function coldInset(name: Name, dec: string, rec: string, throttle: boolean): Promise<any> {
  await quiet(0.1, Infinity, log);
  const at = Date.now();
  const browser = await launch(name);
  const ctx = await browser.newContext();
  try {
    const page = await ctx.newPage();
    if (throttle) {
      const cdp = await ctx.newCDPSession(page);
      await cdp.send("Network.enable");
      await cdp.send("Network.emulateNetworkConditions", SLOW_4G);
    }
    await page.goto(`${server.url}/inset.html?dec=${dec}&rec=/rec/${rec}.chrono`);
    const r: any = await (await page.waitForFunction(() => window.__result, null, { timeout: 120000 })).jsonValue();
    if (r.error) throw new Error(`${dec} ${rec}: ${r.error}`);
    return { ...r, at, end: Date.now(), busy: busyBetween(at, Date.now()) };
  } finally {
    await browser.close();
  }
}

async function inset(name: Name, browser: Browser) {
  const probe = await benchPage(browser);
  const labels = await available(probe, pick(INSET));
  await probe.context().close();
  for (const profile of name === "chromium" ? ["none", "slow4g"] : ["none"]) {
    const throttle = profile === "slow4g";
    // Other work on this machine: no timed measure starts until the load average has settled.
    await settled(2, 300_000, log);
    mark(`inset.${profile}.${name}`);
    const out: any = { browser: name, version: browser.version(), profile, runs: {} };
    const bar = bars.create(recs.length * (runs + 1) * labels.length, 0, { task: `${name} inset ${profile}` });
    for (const rec of recs) {
      // One discarded run a recording, so a cold disk cache lands on none.
      for (const l of labels) {
        await coldInset(name, dir(l), rec, throttle);
        bar.increment();
      }
      for (let r = 0; r < runs; r++) {
        for (const l of rotate(labels, r)) {
          ((out.runs[rec] ??= {})[l] ??= []).push(await coldInset(name, dir(l), rec, throttle));
          bar.increment();
        }
      }
      save(`inset.${profile}.${name}`, out);
    }
    bar.stop();
  }
}

async function blocks(name: Name, browser: Browser, measure: "blocks" | "checksum") {
  const page = await benchPage(browser);
  const labels = await available(page, pick(measure === "blocks" ? BLOCKS : CHECKSUM));
  const list = measure === "blocks" ? recs : ["stock-reads.1024"];
  await settled(2, 300_000, log);
  mark(`${measure}.${name}`);
  const out: any = { browser: name, version: browser.version(), runs: {} };
  for (const rec of list) {
    const n = await setup(page, labels, rec);
    const bar = bars.create((runs + 1) * labels.length * n, 0, { task: `${name} ${measure} ${rec}` });
    const pass = async (l: string) => {
      await quiet(0.1, Infinity, log);
      const at = Date.now();
      const p = await page.evaluate((x) => (globalThis as any).bench.pass(x), l);
      bar.increment(n);
      return { ...p, at, end: Date.now(), busy: busyBetween(at, Date.now()) };
    };
    for (const l of labels) await pass(l); // warm-up, discarded
    for (let r = 0; r < runs; r++) for (const l of rotate(labels, r)) ((out.runs[rec] ??= {})[l] ??= []).push(await pass(l));
    bar.stop();
    save(`${measure}.${name}`, out);
  }
  await page.context().close();
}

async function whole(name: Name, browser: Browser) {
  const page = await benchPage(browser);
  const labels = await available(page, pick(BLOCKS));
  await setup(page, labels, LARGEST);
  await settled(2, 300_000, log);
  mark(`whole.${name}`);
  const out: any = { browser: name, version: browser.version(), recording: LARGEST, runs: {} };
  const bar = bars.create((runs + 1) * labels.length, 0, { task: `${name} whole` });
  const once = async (l: string) => {
    await quiet(0.1, Infinity, log);
    const at = Date.now();
    const w = await page.evaluate((x) => (globalThis as any).bench.whole(x), l);
    bar.increment();
    return { ...w, at, end: Date.now(), busy: busyBetween(at, Date.now()) };
  };
  for (const l of labels) await once(l);
  for (let r = 0; r < runs; r++) for (const l of rotate(labels, r)) (out.runs[l] ??= []).push(await once(l));
  bar.stop();
  await page.context().close();
  save(`whole.${name}`, out);
}

// What was measured: the chrono tree build.sh built from, and the exact modules served.
const sha = (b: string | Buffer) => createHash("sha256").update(b).digest("hex");

// The recordings must be the ones reference.sh hashed, from start to end: a reconversion during a
// run voids it.
function checkRecordings(when: string) {
  for (const rec of ["identify-swd", ...recs]) {
    const want = JSON.parse(readFileSync(`build/reference/${rec}.json`, "utf8")).file_sha256;
    if (sha(readFileSync(`${CONVERTED}/${rec}.chrono`)) !== want) throw new Error(`${rec}.chrono changed ${when}; rerun reference.sh and this`);
  }
}
checkRecordings("since reference.sh hashed it");
save("machine", {
  cpu: cpus()[0].model,
  cores: cpus().length,
  node: process.version,
  date: new Date().toISOString(),
  base: readFileSync("build/base.txt", "utf8").trim(),
  // build/decoder.diff: the uncommitted crates/format and crates/wasm changes when build.sh ran.
  decoderDiff: sha(readFileSync("build/decoder.diff")),
  // build/staged.diff: the part of it staged for the next commit.
  stagedDiff: existsSync("build/staged.diff") ? sha(readFileSync("build/staged.diff")) : null,
  // The transfer sizes of what this run served; a later build.sh rewrites build/sizes.json.
  sizes: JSON.parse(readFileSync("build/sizes.json", "utf8")),
  served: Object.fromEntries(
    readdirSync("build/www/dec").flatMap((d) =>
      readdirSync(`build/www/dec/${d}`)
        .filter((f) => !f.endsWith(".gz"))
        .map((f) => [`${d}/${f}`, sha(readFileSync(`build/www/dec/${d}/${f}`))]),
    ),
  ),
});
for (const name of args.browsers!.split(",") as Name[]) {
  const browser = await launch(name);
  log(`${name} ${browser.version()}`);
  if (measures.includes("correct")) await correct(name, browser);
  if (measures.includes("inset")) await inset(name, browser);
  if (measures.includes("blocks")) await blocks(name, browser, "blocks");
  if (measures.includes("checksum")) await blocks(name, browser, "checksum");
  if (measures.includes("whole")) await whole(name, browser);
  await browser.close();
}
bars.stop();
server.close();
save("cpu", { samples });
checkRecordings("during the run");
console.error("recordings unchanged from start to end");
