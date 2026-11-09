// The benchmark's static server. Like GitHub Pages, it sends .js, .wasm and .html gzipped (from the
// .gz copies bundle.ts writes) and answers single byte ranges, and sends recordings unencoded.
// It isolates the page cross-origin (COOP and COEP), which buys finer timers: without it Firefox
// rounds performance.now() to 1 ms, coarser than a block's decode.
//   node scripts/serve.ts [port]     then open http://localhost:<port>/inset.html?dec=fzstd&rec=/rec/...
import http from "node:http";
import { fileURLToPath } from "node:url";
import sirv from "sirv";

// The converted recordings, served at /rec, which are not in this repository (README.md, "Needs").
export const CONVERTED = process.env.CONVERTED ?? "";
if (!CONVERTED) {
  console.error("set CONVERTED to the folder of converted recordings (README.md, Needs)");
  process.exit(2);
}
const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

function isolate(res: http.ServerResponse, path: string) {
  res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
  res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  res.setHeader("Cache-Control", "no-cache");
  if (/\.(tickwave|bin)$/.test(path)) res.setHeader("Content-Type", "application/octet-stream");
}

export function start(port = 0): Promise<{ url: string; close: () => void }> {
  const opts = { dev: false, etag: false, setHeaders: isolate };
  const mounts: [string, ReturnType<typeof sirv>][] = [
    ["/rec", sirv(CONVERTED, opts)],
    ["/corrupt", sirv(here("../build/corrupt"), opts)],
    ["/ref", sirv(here("../build/reference"), opts)],
    ["", sirv(here("../build/www"), { ...opts, gzip: true })],
  ];
  const server = http.createServer((req, res) => {
    for (const [prefix, handler] of mounts) {
      if (prefix && !req.url!.startsWith(prefix + "/")) continue;
      req.url = req.url!.slice(prefix.length);
      return handler(req, res, () => {
        res.statusCode = 404;
        res.end();
      });
    }
  });
  return new Promise((resolve) =>
    server.listen(port, "127.0.0.1", () => {
      const a = server.address() as { port: number };
      resolve({ url: `http://127.0.0.1:${a.port}`, close: () => server.close() });
    }),
  );
}

if (import.meta.main) {
  const s = await start(Number(process.argv[2] ?? 8080));
  console.log(`serving on ${s.url}`);
}
