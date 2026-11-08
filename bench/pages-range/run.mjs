// Runs check.html in Chromium and Firefox: same-origin on the Pages site, and cross-origin from a
// page served on 127.0.0.1 that fetches the Pages site's files.
//   node run.mjs https://cordlesscoder.github.io/pages-range-test/ > results.json
import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { chromium, firefox } from "playwright";

const site = process.argv[2];
const page = readFileSync(new URL("./check.html", import.meta.url));
const server = createServer((req, res) => {
  res.writeHead(200, { "content-type": "text/html" });
  res.end(page);
}).listen(0, "127.0.0.1");
await new Promise((r) => server.once("listening", r));
const local = `http://127.0.0.1:${server.address().port}/?base=${encodeURIComponent(site)}`;

const out = [];
for (const [name, type] of [["chromium", chromium], ["firefox", firefox]]) {
  const browser = await type.launch();
  for (const [origin, url] of [["same-origin", site], ["cross-origin", local]]) {
    const ctx = await browser.newContext();
    const p = await ctx.newPage();
    p.on("pageerror", (e) => console.error(`${name} ${origin}: page error: ${e}`));
    p.on("console", (m) => m.type() === "error" && console.error(`${name} ${origin}: ${m.text()}`));
    await p.goto(url);
    await p.waitForFunction(() => document.title === "done", null, { timeout: Number(process.env.WAIT_MS ?? 120_000) });
    const rows = JSON.parse(await p.textContent("#out"));
    out.push({ browser: name, version: browser.version(), origin, rows });
    await ctx.close();
  }
  await browser.close();
}
server.close();
console.log(JSON.stringify(out, null, 1));
