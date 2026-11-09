// The browser tests' static server: web/ on two ports, one that honours Range (8124) and one that
// ignores it and always sends the whole file (8125). It logs every request, a worker's included,
// so a test can see what was fetched: GET /__log?tag=X lists the requests whose URL holds X.
//
//   node e2e/server.ts

import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import send from "send";

const root = fileURLToPath(new URL("..", import.meta.url));
interface Entry {
  port: number;
  url: string;
  range: string | null;
  status: number;
}
const log: Entry[] = [];

function serve(port: number, acceptRanges: boolean): void {
  createServer((req, res) => {
    const url = new URL(req.url ?? "/", `http://127.0.0.1:${port}`);
    if (url.pathname === "/__log") {
      const tag = url.searchParams.get("tag") ?? "";
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(log.filter((e) => e.url.includes(tag))));
      return;
    }
    const entry: Entry = { port, url: req.url ?? "/", range: req.headers.range ?? null, status: 0 };
    log.push(entry);
    res.on("finish", () => (entry.status = res.statusCode));
    res.setHeader("cache-control", "no-store");
    send(req, decodeURIComponent(url.pathname), { root, acceptRanges, etag: false, lastModified: false, cacheControl: false })
      .on("error", (e: { status?: number }) => {
        res.statusCode = e.status ?? 500;
        res.end();
      })
      .pipe(res);
  }).listen(port, "127.0.0.1");
}

serve(8124, true);
serve(8125, false);
console.log("serving web/ on 127.0.0.1:8124 (ranges) and :8125 (no ranges)");
