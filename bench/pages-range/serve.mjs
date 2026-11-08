// A local stand-in for a static host, to prove the check can fail. Modes:
//   node serve.mjs DIR PORT ranges   byte ranges, CORS: what the check should pass
//   node serve.mjs DIR PORT whole    ignores Range and sends the whole file
//   node serve.mjs DIR PORT gzip     gzips when asked and ranges over the gzipped bytes, as Pages
//                                    does for some types
import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { join, extname } from "node:path";
import { gzipSync } from "node:zlib";

const [dir, port, mode] = process.argv.slice(2);
const types = { ".html": "text/html", ".json": "application/json" };
createServer((req, res) => {
  if (process.env.LOG && req.headers.range) console.log(`${req.headers["user-agent"]?.match(/Firefox|Chrome/)?.[0]} range ${req.headers.range} accept-encoding ${req.headers["accept-encoding"]}`);
  const path = join(dir, decodeURIComponent(new URL(req.url, "http://x").pathname).replace(/\/$/, "/index.html"));
  let body;
  try {
    body = readFileSync(path);
  } catch {
    res.writeHead(404).end();
    return;
  }
  const headers = { "access-control-allow-origin": "*", "content-type": types[extname(path)] ?? "application/octet-stream" };
  if (mode === "gzip" && /gzip/.test(req.headers["accept-encoding"] ?? "") && !types[extname(path)]) {
    body = gzipSync(body);
    headers["content-encoding"] = "gzip";
  }
  const range = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range ?? "");
  if (range && mode !== "whole") {
    const [a, b] = [Number(range[1]), Math.min(Number(range[2]), body.length - 1)];
    headers["content-range"] = `bytes ${a}-${b}/${body.length}`;
    res.writeHead(206, headers).end(body.subarray(a, b + 1));
  } else {
    res.writeHead(200, headers).end(body);
  }
}).listen(Number(port), "127.0.0.1");
