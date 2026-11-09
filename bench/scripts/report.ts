// Turns results/raw into the Markdown tables RESULTS.md quotes.
//   node scripts/report.ts [results/raw] > results/tables.md
//
// Percentiles are nearest-rank over every sample pooled (runs x blocks for block decode). A
// throughput's "p95" is the slow side: the rate 95 % of samples reach, from the 95th percentile
// of time per item. The A-A floor of a table is the largest relative gap between the medians of
// the two copies of the same decoder over the table's rows; a gap to the reference decoder
// smaller than that is printed as "same".

import { existsSync, readFileSync } from "node:fs";

const raw = process.argv[2] ?? "results/raw";
const load = (n: string) => (existsSync(`${raw}/${n}.json`) ? JSON.parse(readFileSync(`${raw}/${n}.json`, "utf8")) : null);
// The sizes the run served, kept in machine.json; build/sizes.json for runs from before that.
const sizes: Record<string, { raw: number; gzip: number }> =
  load("machine")?.sizes ?? JSON.parse(readFileSync("build/sizes.json", "utf8"));

function pct(xs: number[], p: number): number {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1))];
}
const median = (xs: number[]) => pct(xs, 50);
const f = (x: number, d = 1) => (Number.isFinite(x) ? x.toFixed(d) : "–");
const short = (rec: string) => rec.replace(/\.1024$/, "");

const NAMES: Record<string, string> = {
  // The keys date from when tickwave-wasm inflated with ruzstd; it has used libzstd since 3d17dd1.
  "ruzstd-full": "tickwave-wasm, default build",
  ruzstd: "tickwave-wasm, decoder only",
  "ruzstd#2": "tickwave-wasm, decoder only, A-A copy",
  libzstd: "libzstd in WASM",
  fzstd: "fzstd + TypeScript",
  "fzstd-jsxxh": "fzstd + TypeScript, pure-JS XXH64",
  stream: 'DecompressionStream("zstd") + TypeScript',
  "probe-ruzstd": "probe build, ruzstd (inflate and parse apart)",
  "probe-libzstd": "probe build, libzstd (inflate and parse apart)",
};
const dir = (l: string) => l.split("#")[0];
const transfer = (l: string) => {
  let raw = 0;
  let gzip = 0;
  for (const [k, v] of Object.entries(sizes))
    if (k.startsWith(`dec/${dir(l)}/`)) {
      raw += v.raw;
      gzip += v.gzip;
    }
  return { raw, gzip };
};

function table(head: string[], rows: (string | number)[][]): string {
  return [`| ${head.join(" | ")} |`, `| ${head.map(() => "---").join(" | ")} |`, ...rows.map((r) => `| ${r.join(" | ")} |`)].join("\n");
}

/** Relative A-A floor over rows of {label: samples}. */
/** NaN when no row has both: a run without the A-A pair has no floor. */
function floor(rows: Record<string, number[]>[], a: string, b: string): number {
  let worst = NaN;
  for (const r of rows)
    if (r[a] && r[b]) worst = Math.max(Number.isNaN(worst) ? 0 : worst, Math.abs(median(r[a]) - median(r[b])) / median(r[a]));
  return worst;
}

function versus(x: number, ref: number, fl: number, higherIsBetter: boolean): string {
  const rel = (x - ref) / ref;
  if (Number.isNaN(fl)) return `${rel > 0 ? "+" : ""}${f(100 * rel, 0)} % (no A-A pair)`;
  if (Math.abs(rel) <= fl) return "same";
  const better = higherIsBetter ? rel > 0 : rel < 0;
  return `${rel > 0 ? "+" : ""}${f(100 * rel, 0)} % (${better ? "better" : "worse"})`;
}

const out: string[] = [];
const machine = load("machine");
if (machine) {
  out.push(`Machine: ${machine.cpu}, ${machine.cores} threads; Node ${machine.node}; run started ${machine.date}.`);
  // SHA-256 of nothing: build.sh found no uncommitted decoder changes.
  const clean = machine.decoderDiff === "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
  if (machine.base)
    out.push(
      `Decoders built from tickwave ${machine.base.slice(0, 7)} ${clean ? "with no uncommitted changes to crates/format or crates/wasm" : `with uncommitted crates/format and crates/wasm changes (build/decoder.diff, SHA-256 ${machine.decoderDiff.slice(0, 16)}...)`}. Modules served, SHA-256: ${Object.entries(machine.served as Record<string, string>).filter(([k]) => k.endsWith(".wasm")).map(([k, v]) => `${k} ${v.slice(0, 16)}...`).join(", ")}.\n`,
    );
}

// Every second's busy fraction, from the runner's sampler.
const cpu: [number, number][] = load("cpu")?.samples ?? [];

/**
 * A timed unit's busy fraction: the mean over the seconds it ran, from `at` to `end`. Runs recorded
 * before the runner kept `end` use their own figure, or for a unit under a second (which has
 * none) the second in which it started.
 */
function busyOf(x: { at?: number; end?: number; busy?: number | null }): number {
  if (x.at !== undefined && x.end !== undefined) {
    const xs = cpu.filter(([t]) => t > x.at! && t <= x.end! + 1000).map(([, b]) => b);
    if (xs.length) return xs.reduce((a, b) => a + b, 0) / xs.length;
  }
  if (typeof x.busy === "number") return x.busy;
  const next = cpu.find(([t]) => x.at !== undefined && t > x.at);
  return next ? next[1] : NaN;
}

/** The machine's CPU busy fraction during the timed units, which includes the run itself. */
function busy(xs: number[]): string {
  const ys = xs.filter(Number.isFinite);
  return ys.length ? `Machine busy during the runs (this run included): median ${f(100 * median(ys), 0)} %, max ${f(100 * Math.max(...ys), 0)} %.` : "";
}

// Transfer sizes.
out.push("### Decoder transfer size\n");
out.push(
  table(
    ["decoder", "files", "bytes", "gzip -9"],
    ["ruzstd-full", "ruzstd", "libzstd", "fzstd", "stream"].map((l) => {
      const files = Object.keys(sizes).filter((k) => k.startsWith(`dec/${l}/`) && !k.endsWith(".gz")).map((k) => k.split("/").pop());
      const t = transfer(l);
      return [NAMES[l], files.join(", "), t.raw, t.gzip];
    }),
  ),
);
out.push(`\nEvery decoder also loads the page (inset.html, inset.js: ${sizes["inset.html"].gzip + sizes["inset.js"].gzip} bytes gzipped).\n`);

// PLAN.md §7's bar for a second decoder in JavaScript: the first inset more than 50 ms sooner
// under throttling, or blocks decoded faster than WASM. Rows gathered below, printed near the top.
const barRows: string[][] = [];
const barAt = out.length;

// First inset.
for (const [profile, browsers] of [["none", ["chromium", "firefox"]], ["slow4g", ["chromium"]]] as const) {
  for (const b of browsers) {
    const d = load(`inset.${profile}.${b}`);
    if (!d) continue;
    const recs = Object.keys(d.runs);
    const labels = Object.keys(d.runs[recs[0]]);
    const rows = recs.map((r) => Object.fromEntries(labels.map((l) => [l, d.runs[r][l].map((x: any) => x.drawn)])));
    const fl = floor(rows, "ruzstd", "ruzstd#2");
    out.push(`### First inset, ${b} ${d.version}, ${profile === "none" ? "unthrottled" : 'DevTools "Slow 4G"'}\n`);
    out.push(
      `Milliseconds from navigation start to the drawn inset, median (p95) of ${d.runs[recs[0]][labels[0]].length} cold runs. A-A floor ${f(100 * fl, 1)} %; "vs ruzstd" compares medians with the decoder-only build.\n`,
    );
    const body: string[][] = [];
    recs.forEach((r, i) => {
      for (const l of labels) {
        const xs = rows[i][l];
        const ph = (k: string) => median(d.runs[r][l].map((x: any) => x.phases[k]));
        body.push([
          short(r),
          NAMES[l],
          `${f(median(xs))} (${f(pct(xs, 95))})`,
          f(ph("workerStarted")),
          f(ph("init")),
          f(ph("block")),
          f(ph("decoded") - ph("block")),
          l === "ruzstd" || l === "ruzstd#2" ? "" : versus(median(xs), median(rows[i].ruzstd), fl, false),
        ]);
      }
    });
    out.push(table(["recording", "decoder", "drawn", "worker up", "decoder ready", "block fetched", "decode", "vs ruzstd"], body));
    if (profile === "slow4g")
      recs.forEach((r, i) => {
        if (!rows[i].fzstd) return;
        const js = median(rows[i].fzstd);
        for (const ref of ["ruzstd", "ruzstd-full"]) {
          const wasm = median(rows[i][ref]);
          const noise = fl * wasm;
          const sooner = wasm - js;
          barRows.push([
            `first inset, Slow 4G, ${b}`,
            short(r),
            `fzstd ${f(js, 0)} ms against ${NAMES[ref]} ${f(wasm, 0)} ms: ${sooner >= 0 ? `${f(sooner, 0)} ms sooner` : `${f(-sooner, 0)} ms later`}`,
            `${f(noise, 0)} ms`,
            Math.abs(sooner) <= noise ? "same" : sooner > 50 ? "passes: more than 50 ms sooner" : "does not pass",
          ]);
        }
      });
    const r0 = d.runs[recs[0]][labels[0]][0];
    out.push(`\nBlock: the middle one. cross-origin isolated: ${r0.crossOriginIsolated}.\n`);
    out.push(
      `Bytes fetched a recording, head then the middle block: ${recs.map((r) => `${short(r)} ${d.runs[r][labels[0]][0].headBytes} + ${d.runs[r][labels[0]][0].blockBytes}`).join("; ")}. ${busy(recs.flatMap((r) => labels.flatMap((l) => d.runs[r][l].map(busyOf))))}\n`,
    );
  }
}

// Block decode.
function rate(items: number, ms: number) {
  return items / ms / 1000; // M items/s
}
const blocks = (b: string) => load(`blocks.${b}`);
for (const b of ["chromium", "firefox"]) {
  const d = blocks(b);
  if (!d) continue;
  const recs = Object.keys(d.runs);
  const labels = Object.keys(d.runs[recs[0]]);
  const series = (r: string, l: string, key: "ms" | "rt") => {
    const xs: number[] = [];
    for (const p of d.runs[r][l]) p[key].forEach((t: number, i: number) => xs.push(rate(p.edges[i] + p.samples[i], t)));
    return xs.filter((x) => Number.isFinite(x) && x > 0);
  };
  const rowsW = recs.map((r) => Object.fromEntries(labels.map((l) => [l, series(r, l, "ms")])));
  const rowsR = recs.map((r) => Object.fromEntries(labels.map((l) => [l, series(r, l, "rt")])));
  const flW = floor(rowsW, "ruzstd", "ruzstd#2");
  const flR = floor(rowsR, "ruzstd", "ruzstd#2");
  out.push(`### Block decode, ${b} ${d.version}\n`);
  out.push(
    `Millions of edges a second (session50: of samples), median (p95, slow side) over ${d.runs[recs[0]][labels[0]].length} runs of every block. "In worker": decode inside the worker, to typed arrays. "Round trip": the page's request to the arrays arriving back, the worker hop included. A-A floor ${f(100 * flW, 1)} % in worker, ${f(100 * flR, 1)} % round trip.\n`,
  );
  const body: string[][] = [];
  recs.forEach((r, i) => {
    for (const l of labels) {
      const w = rowsW[i][l];
      const rt = rowsR[i][l];
      const hop: number[] = [];
      for (const p of d.runs[r][l]) p.rt.forEach((t: number, k: number) => hop.push(t - p.ms[k]));
      body.push([
        short(r),
        NAMES[l],
        `${f(median(w))} (${f(pct(w, 5))})`,
        `${f(median(rt))} (${f(pct(rt, 5))})`,
        f(median(hop), 2),
        l.startsWith("ruzstd") ? "" : versus(median(w), median(rowsW[i].ruzstd), flW, true),
      ]);
    }
  });
  out.push(table(["recording", "decoder", "in worker", "round trip", "hop ms", "in worker vs ruzstd"], body));
  out.push(`\n${busy(recs.flatMap((r) => labels.flatMap((l) => d.runs[r][l].map(busyOf))))}`);
  recs.forEach((r, i) => {
    if (!rowsW[i].fzstd) return;
    const js = median(rowsW[i].fzstd);
    const wasm = median(rowsW[i].ruzstd);
    barRows.push([`block decode in worker, ${b}`, short(r), `fzstd ${f(js)} against ${f(wasm)} M/s`, `${f(100 * flW, 1)} %`, js > wasm * (1 + flW) ? "JavaScript faster" : js < wasm * (1 - flW) ? "WASM faster" : "same"]);
  });

  // Where the time goes inside the worker.
  const phaseRows: string[][] = [];
  for (const l of labels) {
    if (l.endsWith("#2")) continue;
    const per: Record<string, number[]> = {};
    for (const r of recs) for (const p of d.runs[r][l]) for (const [k, v] of Object.entries(p.phases as Record<string, number[]>)) (per[k] ??= []).push(...v);
    phaseRows.push([NAMES[l], Object.entries(per).map(([k, v]) => `${k} ${f(median(v), 2)}`).join(", ")]);
  }
  out.push(`\nMedian ms a block by step, every recording pooled:\n`);
  out.push(table(["decoder", "steps"], phaseRows));
  out.push("");
}

// Checksum.
for (const b of ["chromium", "firefox"]) {
  const d = load(`checksum.${b}`);
  if (!d) continue;
  const r = Object.keys(d.runs)[0];
  out.push(`### Checksum (XXH64 over the decoded block), ${b}\n`);
  const body = Object.keys(d.runs[r]).map((l) => {
    const xs = d.runs[r][l].flatMap((p: any) => p.phases.checksum);
    const all = d.runs[r][l].flatMap((p: any) => p.ms);
    return [NAMES[l], f(median(xs), 2), f(pct(xs, 95), 2), f(median(all), 2)];
  });
  out.push(`${short(r)}, ${d.runs[r][Object.keys(d.runs[r])[0]].length} runs of each of its 1 MiB blocks (the last is shorter).\n`);
  out.push(table(["decoder", "checksum ms median", "p95", "whole block ms median"], body));
  out.push("");
}

// Whole file.
const wholeRows: string[][] = [];
let wholeEdges = 0;
const nativeT = existsSync("results/native") ? JSON.parse(readFileSync(`results/native/gcc-swd-matrix-capture100000.1024.json`, "utf8")) : null;
for (const b of ["chromium", "firefox"]) {
  const d = load(`whole.${b}`);
  if (!d) continue;
  const labels = Object.keys(d.runs);
  const fl = Math.abs(median(d.runs.ruzstd.map((x: any) => x.ms)) - median(d.runs["ruzstd#2"].map((x: any) => x.ms))) / median(d.runs.ruzstd.map((x: any) => x.ms));
  wholeEdges = d.runs.ruzstd[0].edges;
  for (const l of labels) {
    const ms = d.runs[l].map((x: any) => x.ms);
    const edges = d.runs[l][0].edges;
    wholeRows.push([
      `${b} ${d.version}`,
      NAMES[l],
      `${f(median(ms) / 1000, 2)} (${f(pct(ms, 95) / 1000, 2)})`,
      f(edges / median(ms) / 1000, 0),
      l.startsWith("ruzstd") ? `A-A ${f(100 * fl, 1)} %` : versus(median(ms), median(d.runs.ruzstd.map((x: any) => x.ms)), fl, false),
      `${f(100 * median(d.runs[l].map(busyOf)), 0)} %`,
    ]);
  }
}
if (nativeT) {
  const ms = nativeT.whole_ns.map((n: number) => n / 1e6);
  const edges = nativeT.edges.reduce((a: number, b: number) => a + b, 0);
  wholeEdges ||= edges;
  wholeRows.push(["native", "Head::decode_block (ruzstd), one thread", `${f(median(ms) / 1000, 2)} (${f(pct(ms, 95) / 1000, 2)})`, f(edges / median(ms) / 1000, 0), "", ""]);
}
if (wholeRows.length) {
  out.push(`### The largest recording decoded whole (gcc-swd-matrix-capture100000, 742 blocks, ${wholeEdges.toLocaleString("en")} edges)\n`);
  out.push("Seconds, median (p95) of the runs, every block in turn inside one worker; M edges a second at the median; the machine's median busy fraction during the runs, this run included.\n");
  out.push(table(["browser", "decoder", "seconds", "M edges/s", "vs ruzstd", "machine busy"], wholeRows));
  out.push("");
}

// Native per-block reference.
if (existsSync("results/native")) {
  const body: string[][] = [];
  for (const r of ["gcc-swd-reads", "stock-reads", "swo-24000000", "gcc-oscan1-probe-rs", "gcc-swd-matrix-capture100000", "session50"]) {
    const p = `results/native/${r}.1024.json`;
    if (!existsSync(p)) continue;
    const t = JSON.parse(readFileSync(p, "utf8"));
    const xs: number[] = [];
    for (const run of t.ns) run.forEach((ns: number, i: number) => xs.push((t.edges[i] + t.samples[i]) / (ns / 1e6) / 1000));
    body.push([r, `${f(median(xs))} (${f(pct(xs, 5))})`]);
  }
  out.push("### Native reference, Head::decode_block\n");
  out.push(`M items a second, median (p95, slow side) over ${JSON.parse(readFileSync("results/native/gcc-swd-reads.1024.json", "utf8")).ns.length} runs of every block.\n`);
  out.push(table(["recording", "native"], body));
  out.push("");
}

// Correctness.
for (const b of ["chromium", "firefox"]) {
  const d = load(`correct.${b}`);
  if (!d) continue;
  out.push(`### Correctness, ${b} ${d.version}\n`);
  out.push(
    `DecompressionStream("zstd"): ${d.defaultFeatures.stream ? "yes" : "no"} by default${b === "firefox" ? `, ${d.features.stream ? "yes" : "no"} with dom.compression_streams.zstd.enabled` : ""}. Cross-origin isolated: ${d.features.crossOriginIsolated}.\n`,
  );
  const per: Record<string, { blocks: number; bad: number }> = {};
  for (const [k, v] of Object.entries(d.hashes as Record<string, { blocks: number; mismatched: unknown[] }>)) {
    const l = k.split(" ")[1];
    per[l] ??= { blocks: 0, bad: 0 };
    per[l].blocks += v.blocks;
    per[l].bad += v.mismatched.length;
  }
  const ov: Record<string, { blocks: number; bad: number } | null> = {};
  for (const [k, v] of Object.entries((d.overview ?? {}) as Record<string, { blocks: number; mismatched: unknown[] } | null>)) {
    const l = k.split(" ")[1];
    if (v === null) ov[l] = null;
    else {
      ov[l] ??= { blocks: 0, bad: 0 };
      ov[l].blocks += v.blocks;
      ov[l].bad += v.mismatched.length;
    }
  }
  out.push(
    table(
      ["decoder", "blocks hashed", "differ from native", "overview blocks hashed", "differ from native"],
      Object.entries(per).map(([l, v]) => [NAMES[l], v.blocks, v.bad, ov[l] ? ov[l]!.blocks : "no API", ov[l] ? ov[l]!.bad : ""]),
    ),
  );
  out.push("");
  out.push(
    table(
      ["recording", "corruption", "refuse at", "native refusal", ...Object.keys(per).map((l) => NAMES[l])],
      Object.values(
        (d.corrupt as any[]).reduce((acc: any, c: any) => {
          const k = `${c.file} ${c.kind}`;
          acc[k] ??= { file: short(c.file), kind: c.kind, expected: c.expected ?? "block", native: c.native, by: {} };
          const where = c.stage ? ` at ${c.stage}${c.expected && c.stage !== c.expected ? ` (expected ${c.expected})` : ""}` : "";
          acc[k].by[c.decoder] = c.refused ? `refused${where}: ${String(c.error).split("\n")[0].replace(/\|/g, "/").slice(0, 90)}` : "accepted";
          return acc;
        }, {}),
      ).map((x: any) => [x.file, x.kind, x.expected, x.native, ...Object.keys(per).map((l) => x.by[l] ?? "")]),
    ),
  );
  out.push("");
}

if (barRows.length)
  out.splice(
    barAt,
    0,
    "### Against the bar (PLAN.md §7)\n",
    "A JavaScript decoder (fzstd) against tickwave-wasm. A gap within the A-A floor is \"same\".\n",
    table(["measure", "recording", "medians", "A-A floor", "verdict"], barRows),
    "",
  );
console.log(out.join("\n"));
