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

/// The pages of a table that hold keys [k0, k1), each as its seven numbers, the directories on the
/// way handed in as `look` asks for them.
function located(rec, file, look) {
  for (let pages = look(); ; pages = look()) {
    if (pages) return Array.from({ length: pages.length / 7 }, (_, i) => pages.subarray(7 * i, 7 * i + 7));
    const m = rec.missing();
    if (m.length === 0) throw new Error("a look-up stopped short of its pages");
    for (let i = 0; i < m.length; i += 2) rec.addDirectory(m[i], file.subarray(m[i], m[i + 1]));
  }
}

/// Everything a page reads of `file` a page at a time, as the web's worker does: each channel's
/// block pages, its blocks (a channel whose list is in pages checked against all of it), its list
/// pages, its strip, and each level's pages and overview blocks.
function readPaged(file) {
  const rec = open(file);
  const header = JSON.parse(rec.header);
  const C = header.channels.length;
  header.channels.forEach((c, k) => {
    let list;
    if (c.gap_count !== undefined || c.break_count !== undefined) {
      list = located(rec, file, () => rec.listPages(k, -(2 ** 53), 2 ** 53)).flatMap((at) => [...rec.listPage(k, at, file.subarray(at[1], at[1] + at[2]))]);
    }
    for (const at of located(rec, file, () => rec.blockPages(k, 0, header.length))) {
      const blocks = rec.blockPage(k, at, file.subarray(at[1], at[1] + at[2]));
      for (let i = 0; i < blocks.length; i += 8) {
        const b = blocks.subarray(i, i + 8);
        rec.decodeBlockAt(k, b, file.subarray(b[3], b[3] + b[4]), list && new Float64Array(list)).free();
      }
    }
  });
  for (let o = 0; o < C + (header.derived ?? []).length; o++) {
    rec.strip(o).free();
    const shifts = rec.shifts(o);
    for (let li = 0; li + 1 < shifts.length; li++) {
      for (const at of located(rec, file, () => rec.levelPages(o, li, 0, 2 ** 53))) {
        const rows = rec.levelPage(o, li, at, file.subarray(at[1], at[1] + at[2]));
        for (let i = 0; i < rows.length; i += 5) {
          const r = rows.subarray(i, i + 5);
          rec.decodeOverviewAt(o, li, r, file.subarray(r[1], r[1] + r[2])).free();
        }
      }
    }
  }
  const bundle = rec.bundleRange();
  if (bundle.length > 0) rec.bundleFiles(file.subarray(bundle[0], bundle[1]));
  rec.free();
}

for (const name of readdirSync(new URL("crafted/", fixtures)).filter((n) => n.endsWith(".tickwave")).sort()) {
  for (const [how, read] of [
    ["", readAll],
    [", a page at a time", readPaged],
  ]) {
    const e = attempt(`${name}${how}`, () => read(readFileSync(new URL(`crafted/${name}`, fixtures))));
    if (e) {
      console.log(`${name}${how}: refused: ${e.message}`);
    } else {
      console.log(`${name}${how}: READ THROUGH`);
      failed++;
    }
  }
}
// The good fixtures read through a page at a time, energy's bundle and derived channel among them.
for (const name of ["identify-swd.tickwave", "identify-jtag.tickwave", "swd50-cut.tickwave", "energy.tickwave"]) {
  const e = attempt(`${name}, a page at a time`, () => readPaged(readFileSync(new URL(name, fixtures))));
  console.log(`${name}, a page at a time: ${e ? `REFUSED: ${e.message}` : "read"}`);
  if (e) failed++;
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
  "blockPages(99, ..)": () => good.blockPages(99, 0, 1),
  "levelPages(0, 99, ..)": () => good.levelPages(0, 99, 0, 1),
  "listPages(0, ..), no list in pages": () => good.listPages(0, 0, 1),
  "blockPage(0, six numbers, ..)": () => good.blockPage(0, new Float64Array(6), swd),
  "blockPage(0, .., a corrupt page)": () => {
    const [at] = located(good, swd, () => good.blockPages(0, 0, 1));
    good.blockPage(0, at, corrupt(swd.subarray(at[1], at[1] + at[2])));
  },
  "blockPage(0, .., another channel's page)": () => {
    const [at] = located(good, swd, () => good.blockPages(1, 0, 1));
    good.blockPage(0, at, swd.subarray(at[1], at[1] + at[2]));
  },
  "levelPage(0, 99, ..)": () => good.levelPage(0, 99, new Float64Array(7), swd),
  "listPage(0, ..), no list in pages": () => good.listPage(0, new Float64Array(7), swd),
  "decodeBlockAt(99, ..)": () => good.decodeBlockAt(99, new Float64Array(8), new Uint8Array(4)),
  "decodeBlockAt(0, seven numbers, ..)": () => good.decodeBlockAt(0, new Float64Array(7), new Uint8Array(4)),
  "decodeBlockAt(0, .., a short frame)": () => good.decodeBlockAt(0, Float64Array.of(0, 1, 0, 527, 1727, 4000, 0, NaN), new Uint8Array(4)),
  "decodeBlockAt(0, .., a list of two numbers)": () => good.decodeBlockAt(0, Float64Array.of(0, 1, 0, 527, 1727, 4000, 0, NaN), swd.subarray(527, 2254), new Float64Array(2)),
  "decodeOverviewAt(0, 99, ..)": () => good.decodeOverviewAt(0, 99, new Float64Array(5), new Uint8Array(4)),
  "productValues(99, ..)": () => good.productValues(99, new Float32Array(1), new Float32Array(1)),
  "bundleFiles(..), no bundle": () => good.bundleFiles(new Uint8Array(4)),
  "footerStart(a byte past the trailer)": () => footerStart(swd.subarray(0, swd.length - 1), swd.length - 1),
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
