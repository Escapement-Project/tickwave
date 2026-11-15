// Runs recordings that must be refused through the WASM build, and checks each is refused with an
// error, never a panic: a panic traps the module, and with it every recording on the page.
//   node crates/wasm/refusals.mjs
// - Each file in web/fixtures/crafted/, read as a page reads it: refused somewhere on the way.
// - identify-swd.tickwave with a corrupt index page handed in: refused, and the recording then
//   answers every call with an answer or an error, and reads once the right page comes in.
// - Out-of-range channels, blocks, levels and rows asked of a good recording: errors.
import { readFileSync, readdirSync } from "node:fs";
import { initSync, footerStart, Recording } from "./pkg/escapement_tickwave_wasm.js";

initSync({ module: readFileSync(new URL("./pkg/escapement_tickwave_wasm_bg.wasm", import.meta.url)) });
const fixtures = new URL("../../web/fixtures/", import.meta.url);
let failed = 0;

/// Runs `f`; a panic fails the check, an error is returned, anything else is `null`.
function attempt(what, f) {
  try {
    f();
    return null;
  } catch (e) {
    if (e instanceof WebAssembly.RuntimeError) {
      console.log(`${what}: PANICKED: ${e.message}`);
      failed++;
      return null;
    }
    return e;
  }
}

function open(file) {
  const from = footerStart(file.subarray(Math.max(0, file.length - 32768)), file.length);
  return new Recording(file.subarray(from), file.length);
}

/// Hands in every page `rec` asks for, each through `edit`; the ranges asked, in order.
function handIn(rec, file, edit = (b) => b) {
  const asked = [];
  for (let ranges = rec.indexRanges(); ranges.length > 0; ranges = rec.indexRanges()) {
    for (let i = 0; i < ranges.length; i += 2) {
      asked.push([ranges[i], ranges[i + 1]]);
      rec.addIndexPage(ranges[i], edit(file.subarray(ranges[i], ranges[i + 1])));
    }
  }
  return asked;
}

/// Everything a page reads of `file`: the footer, the index, every block, strip and overview row.
function readAll(file) {
  const rec = open(file);
  handIn(rec, file);
  const header = JSON.parse(rec.header);
  const overview = header.channels.length + (header.derived ?? []).length;
  header.channels.forEach((_, c) => {
    for (let i = 0; i < rec.blockCount(c); i++) {
      const [from, to] = rec.blockRange(c, i);
      rec.decodeBlock(c, i, file.subarray(from, to)).free();
    }
  });
  for (let o = 0; o < overview; o++) {
    rec.strip(o).free();
    const shifts = rec.shifts(o);
    for (let li = 0; li + 1 < shifts.length; li++) {
      const [r0, r1] = rec.overviewRowsCovering(o, li, 0, header.length);
      for (let r = r0; r < r1; r++) {
        const [from, to] = rec.overviewRowRange(o, li, r);
        rec.decodeOverviewRow(o, li, r, file.subarray(from, to)).free();
      }
    }
  }
  rec.free();
}

for (const name of readdirSync(new URL("crafted/", fixtures)).filter((n) => n.endsWith(".tickwave")).sort()) {
  const e = attempt(name, () => readAll(readFileSync(new URL(`crafted/${name}`, fixtures))));
  if (e) {
    console.log(`${name}: refused: ${e.message}`);
  } else {
    console.log(`${name}: READ THROUGH`);
    failed++;
  }
}

const swd = readFileSync(new URL("identify-swd.tickwave", fixtures));
// Each page with its last entry's byte changed, a copy: its check no longer matches.
const corrupt = (b) => {
  const c = new Uint8Array(b);
  c[c.length - 9] ^= 0x40;
  return c;
};
const rec = open(swd);
const asked = [];
const refused = attempt("a corrupt page", () => {
  for (let ranges = rec.indexRanges(); ranges.length > 0; ranges = rec.indexRanges()) {
    for (let i = 0; i < ranges.length; i += 2) {
      asked.push([ranges[i], ranges[i + 1]]);
      rec.addIndexPage(ranges[i], corrupt(swd.subarray(ranges[i], ranges[i + 1])));
    }
  }
});
console.log(`a corrupt page: ${refused ? `refused: ${refused.message}` : "READ THROUGH"}`);
if (!refused) failed++;
const calls = {
  "indexRanges again": () => rec.indexRanges(),
  "blockCount": () => rec.blockCount(0),
  "blockRange": () => rec.blockRange(0, 0),
  "blocksCovering": () => rec.blocksCovering(0, 0, 1000),
  "strip": () => rec.strip(0).free(),
  "shifts": () => rec.shifts(0),
  "overviewRowsCovering": () => rec.overviewRowsCovering(0, 0, 0, 1000),
  "header": () => rec.header,
};
for (const [what, f] of Object.entries(calls)) {
  const e = attempt(`after a corrupt page, ${what}`, f);
  console.log(`after a corrupt page, ${what}: ${e ? `error: ${e.message}` : "answered"}`);
}
// The right pages in place of the corrupt ones: the recording reads.
const e = attempt("the right pages after a corrupt one", () => {
  for (const [from, to] of asked) rec.addIndexPage(from, swd.subarray(from, to));
  handIn(rec, swd);
  if (rec.blockCount(0) < 1) throw new Error("no block");
});
console.log(`the right pages after a corrupt one: ${e ? `error: ${e.message}` : "read"}`);
if (e) failed++;

const good = open(swd);
handIn(good, swd);
const wrong = {
  "blockCount(99)": () => good.blockCount(99),
  "blockRange(0, 99999)": () => good.blockRange(0, 99999),
  "blocksCovering(99, ..)": () => good.blocksCovering(99, 0, 1),
  "decodeBlock(0, 99999, ..)": () => good.decodeBlock(0, 99999, new Uint8Array(4)),
  "decodeBlock(0, 0, short)": () => good.decodeBlock(0, 0, new Uint8Array(4)),
  "strip(99)": () => good.strip(99),
  "shifts(99)": () => good.shifts(99),
  "overviewRowsCovering(0, 99, ..)": () => good.overviewRowsCovering(0, 99, 0, 1),
  "overviewRowsCovering(99, 0, ..)": () => good.overviewRowsCovering(99, 0, 0, 1),
  "overviewRowRange(0, 99, 0)": () => good.overviewRowRange(0, 99, 0),
  "decodeOverviewRow(0, 99, 0, ..)": () => good.decodeOverviewRow(0, 99, 0, new Uint8Array(4)),
};
for (const [what, f] of Object.entries(wrong)) {
  const e = attempt(what, f);
  console.log(`${what}: ${e ? `error: ${e.message}` : "ANSWERED"}`);
  if (!e) failed++;
}
if (failed > 0) {
  console.log(`${failed} failed`);
  process.exit(1);
}
