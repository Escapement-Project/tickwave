// One cold inset per decoder in each browser, printed: a quick check that everything loads.
//   node scripts/smoke.ts [recording]
import { chromium, firefox } from "playwright";
import { start } from "./serve.ts";

const rec = process.argv[2] ?? "gcc-swd-reads.1024.chrono";
const server = await start();
for (const type of [chromium, firefox]) {
  const browser = await type.launch();
  console.log(type.name(), browser.version());
  for (const dec of ["ruzstd-full", "ruzstd", "libzstd", "fzstd", "stream"]) {
    const ctx = await browser.newContext();
    const page = await ctx.newPage();
    page.on("pageerror", (e) => console.log("  pageerror", e.message));
    await page.goto(`${server.url}/inset.html?dec=${dec}&rec=/rec/${rec}`);
    const r: any = await page.waitForFunction(() => window.__result, null, { timeout: 30000 }).then((h) => h.jsonValue());
    console.log(" ", dec.padEnd(12), r.error ?? `drawn ${r.drawn.toFixed(1)} ms, block ${r.block}, isolated ${r.crossOriginIsolated}, phases ${JSON.stringify(Object.fromEntries(Object.entries(r.phases).map(([k, v]: any) => [k, +v.toFixed(1)])))}`);
    await ctx.close();
  }
  await browser.close();
}
server.close();
