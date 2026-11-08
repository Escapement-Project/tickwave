# GitHub Pages and byte ranges, 2026-11-08

A reader of a chrono recording fetches the head, then only the blocks a view covers, by HTTP
Range. This checks that GitHub Pages serves those ranges unchanged to the browsers chrono targets.

## Result

GitHub Pages serves byte ranges correctly to Chromium and Firefox, from the same origin and across
origins: 144 of 144 ranges right, byte for byte. chrono can rely on Range requests on Pages. The
fallback of one file per block is not needed there.

## What was checked

A throwaway public repository held random files: 1 MiB each as `small.chrono`, `small.bin` and
`small.zst`, and a 60 MiB `large.chrono`, larger than the largest recording so far. For each file the
page fetched three ranges: the first 64 KiB (a reader's first fetch), a block from the middle,
and the last 1,000 bytes. Each passed only if it came back as 206, with no `Content-Encoding`, the
right length, and the SHA-256 of the local file's same bytes. Three passes ran: one with
`cache: "no-store"`, two with the browser's cache allowed.

| Browser | Same origin | Cross origin |
| --- | --- | --- |
| Chromium 158.0.8127.18 | 36 of 36 | 36 of 36 |
| Firefox 159.0 | 36 of 36 | 36 of 36 |

The cross-origin page ran on `127.0.0.1` and read `https://cordlesscoder.github.io/`. Pages sends
`Access-Control-Allow-Origin: *`, and a single range needs no preflight.

## Pages gzips binary files

Pages serves an unknown extension as `application/octet-stream`. When a request accepts gzip, it
compresses `.chrono`, `.bin` and `.zst` alike, and the range then counts bytes of the gzipped
stream. curl with `Accept-Encoding: gzip, deflate, br, zstd` got `Content-Encoding: gzip` and a
total of 1,048,914 bytes for a 1,048,576-byte file.

Browsers are not affected, because they ask for ranges uncompressed. Chromium and Firefox both
send `Accept-Encoding: identity` with every request that carries a Range header, as the Fetch
standard specifies. A local server logged that header for every range request (`serve.mjs` with
`LOG=1`). With `identity`, Pages returned the plain bytes and the right `Content-Range` total. A
non-browser client reading recordings by range must send `Accept-Encoding: identity` too.

## How the check was proven before use

`dry-run.sh` runs the same check against `serve.mjs`, a local stand-in host with three modes.
Every row must pass with real ranges, and fail when the server ignores Range:

| Local server | Chromium | Firefox |
| --- | --- | --- |
| ranges and CORS | 72 of 72 pass | 72 of 72 pass |
| Range ignored, whole file sent | 0 of 72 pass | 0 of 72 pass |
| gzip before ranging, when accepted | 72 of 72 pass | 72 of 72 pass |

The third row passes because browsers never accept gzip on a range request. That is the finding
above, not a gap in the check.

## Not checked

- A cold CDN cache in a browser. Every browser request hit Fastly's cache (`x-cache: HIT`),
  because curl had fetched the same ranges first. Those curl requests were cache misses, and they
  returned the right ranges.
- WebKit and Safari.
- Several ranges in one request (`bytes=0-9,20-29`); chrono's reader never sends one.
- A file over 60 MiB. Pages' published-site limit is 1 GB, and a repository refuses files over
  100 MB.

## Rerun

```sh
npm install                      # Playwright, at its current version
uv run make-site.py SITE_DIR     # random files, files.json, index.html
./dry-run.sh SITE_DIR            # the check against local servers
# publish SITE_DIR on the host, then:
node run.mjs https://HOST/PATH/ > results.json
```
