// The browser tests' static server: web/ on three ports, one that honours Range (8124), one that
// ignores it and always sends the whole file (8125), and another site that honours Range and sends
// Access-Control-Allow-Origin: *, as GitHub Pages does (8126). It logs every request, a worker's
// included, so a test can see what was fetched: GET /__log?tag=X lists the requests whose URL
// holds X.
// A request URL with `after=N` makes every Range read past the Nth for its tag wait `stall`
// milliseconds, then fail with status `fail` or be answered; with `until=M`, only the reads up to
// the Mth. /named/<path>/<name> serves web/<path> under another name: a recording without a title
// of its own takes its file's name as one.
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
const rangeReads = new Map<string, number>();

function serve(port: number, acceptRanges: boolean, cors = false): void {
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
    if (cors) res.setHeader("access-control-allow-origin", "*");
    const named = /^\/named(\/.+)\/[^/]+$/.exec(url.pathname);
    const path = decodeURIComponent(named ? named[1] : url.pathname);
    const answer = () =>
      send(req, path, { root, acceptRanges, etag: false, lastModified: false, cacheControl: false })
        .on("error", (e: { status?: number }) => {
          res.statusCode = e.status ?? 500;
          res.end();
        })
        .pipe(res);
    const after = url.searchParams.get("after");
    if (after !== null && req.headers.range) {
      const tag = url.searchParams.get("tag") ?? "";
      const n = (rangeReads.get(tag) ?? 0) + 1;
      rangeReads.set(tag, n);
      const until = url.searchParams.get("until");
      if (n > Number(after) && (until === null || n <= Number(until))) {
        const fail = url.searchParams.get("fail");
        const failNow = () => {
          res.statusCode = Number(fail);
          res.end();
        };
        setTimeout(fail === null ? answer : failNow, Number(url.searchParams.get("stall") ?? 0));
        return;
      }
    }
    answer();
  }).listen(port, "127.0.0.1");
}

serve(8124, true);
serve(8125, false);
serve(8126, true, true);
console.log("serving web/ on 127.0.0.1:8124 (ranges), :8125 (no ranges) and :8126 (ranges, CORS)");
