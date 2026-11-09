// Bundles the workers and pages into build/www, gzips what a server would, and writes each
// decoder's transfer size to build/sizes.json.
import { build } from "esbuild";
import { copyFileSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { gzipSync, constants } from "node:zlib";
import { join } from "node:path";

const www = "build/www";
const entries = readdirSync("src/entry").map((f) => f.replace(/\.ts$/, ""));
await build({
  entryPoints: Object.fromEntries([
    ...entries.map((e) => [`dec/${e}/worker`, `src/entry/${e}.ts`]),
    ["inset", "src/page-inset.ts"],
    ["bench", "src/page-bench.ts"],
  ]),
  outdir: www,
  bundle: true,
  format: "esm",
  minify: true,
  target: "es2022",
  logLevel: "warning",
});
for (const p of ["inset.html", "bench.html"]) copyFileSync(join("pages", p), join(www, p));

// gzip -9, the most a server would spend; GitHub Pages' own level is unknown.
const sizes: Record<string, { raw: number; gzip: number }> = {};
const walk = (d: string): string[] =>
  readdirSync(d).flatMap((f) => (statSync(join(d, f)).isDirectory() ? walk(join(d, f)) : [join(d, f)]));
for (const f of walk(www)) {
  if (!/\.(js|wasm|html)$/.test(f)) continue;
  const raw = readFileSync(f);
  const gz = gzipSync(raw, { level: constants.Z_BEST_COMPRESSION });
  writeFileSync(f + ".gz", gz);
  sizes[f.slice(www.length + 1)] = { raw: raw.length, gzip: gz.length };
}
writeFileSync("build/sizes.json", JSON.stringify(sizes, null, 1));
for (const [k, v] of Object.entries(sizes)) console.log(`${k.padEnd(48)} ${String(v.raw).padStart(9)} ${String(v.gzip).padStart(9)}`);
