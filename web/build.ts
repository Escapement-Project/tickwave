// Builds dist/chrono.js (the elements, one ES module) and dist/dev-loader.js (the dev page's
// fixture loader, which imports "chrono" through the page's import map).
//
//   node build.ts                 build once
//   node build.ts --serve [PORT]  rebuild on change and serve web/ (the dev page is /dev/)

import * as esbuild from "esbuild";

const serve = process.argv.includes("--serve");
const port = Number(process.argv[process.argv.indexOf("--serve") + 1]) || 8123;

const options: esbuild.BuildOptions = {
  entryPoints: { chrono: "src/index.ts", "dev-loader": "dev/loader.ts" },
  outdir: "dist",
  bundle: true,
  format: "esm",
  target: "es2022",
  external: ["chrono"],
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
