// Is a fresh context as cold as a fresh browser? Times the decoder's fetch-and-compile step
// (worker message to decoder ready) both ways, alternating.
//   node scripts/cold-check.ts [decoder] [runs]
import { chromium, firefox, type Browser } from "playwright";
import { start } from "./serve.ts";

const dec = process.argv[2] ?? "ruzstd-full";
const runs = Number(process.argv[3] ?? 8);
const server = await start();
const median = (xs: number[]) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
async function once(browser: Browser) {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto(`${server.url}/inset.html?dec=${dec}&rec=/rec/gcc-swd-reads.1024.chrono`);
  const r: any = await (await page.waitForFunction(() => window.__result)).jsonValue();
  await ctx.close();
  return r.phases.init - r.phases.message;
}
for (const type of [chromium, firefox]) {
  const fresh: number[] = [];
  const reused: number[] = [];
  const shared = await type.launch();
  await once(shared);
  for (let i = 0; i < runs; i++) {
    const b = await type.launch();
    fresh.push(await once(b));
    await b.close();
    reused.push(await once(shared));
  }
  await shared.close();
  console.log(type.name(), dec, "decoder ready, ms: fresh browser", median(fresh).toFixed(1), JSON.stringify(fresh.map((x) => +x.toFixed(1))), "| new context", median(reused).toFixed(1), JSON.stringify(reused.map((x) => +x.toFixed(1))));
}
server.close();
