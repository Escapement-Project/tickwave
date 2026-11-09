// Builds dist/: tickwave.js (the elements, one ES module), tickwave-chart.js (the chart element,
// its own module), tickwave-worker.js (the decoder's worker), tickwave.wasm (the decoder, copied
// from crates/wasm/pkg), and dev-loader.js (the dev page's fixture loader, which imports
// "tickwave" through the page's import map).
//
//   node build.ts                 build once
//   node build.ts --serve [PORT]  rebuild on change and serve web/ (the dev page is /dev/)

import * as esbuild from "esbuild";
import { copyFileSync, existsSync, mkdirSync } from "node:fs";

const serve = process.argv.includes("--serve");
const port = Number(process.argv[process.argv.indexOf("--serve") + 1]) || 8123;

const wasm = new URL("../crates/wasm/pkg/tickwave_wasm_bg.wasm", import.meta.url);
if (!existsSync(wasm)) {
  console.error("crates/wasm/pkg has no build: run crates/wasm/build.sh first");
  process.exit(1);
}
mkdirSync("dist", { recursive: true });
copyFileSync(wasm, "dist/tickwave.wasm");

const options: esbuild.BuildOptions = {
  entryPoints: { tickwave: "src/index.ts", "tickwave-chart": "src/chart/index.ts", "tickwave-worker": "src/wasm/worker.ts", "dev-loader": "dev/loader.ts" },
  outdir: "dist",
  bundle: true,
  format: "esm",
  target: "es2022",
  external: ["tickwave"],
  minify: !serve,
  sourcemap: true,
  metafile: true,
  logLevel: "info",
};

if (serve) {
  const ctx = await esbuild.context(options);
  await ctx.watch();
  const { hosts, port: bound } = await ctx.serve({ servedir: ".", host: "127.0.0.1", port });
  console.log(`dev page: http://${hosts[0]}:${bound}/dev/`);
} else {
  const result = await esbuild.build(options);
  for (const [file, out] of Object.entries(result.metafile!.outputs)) {
    if (file.endsWith(".js")) console.log(`${file}: ${out.bytes} bytes`);
  }
}
