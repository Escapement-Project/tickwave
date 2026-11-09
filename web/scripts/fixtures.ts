// Rebuilds the ignored fixtures in fixtures-local/ and their annotations. Run from web/:
//
//   node scripts/fixtures.ts [--exports DIR] [--energy CSV] [--converted DIR]
//
// - <name>.chronofx: cut from the Saleae exports and verge's energy CSV (extract-fixtures.ts).
// - <name>.chrono: a link to the chrono session's converted recording, where one exists, so the
//   dev page can draw its stress sections through the WASM decoder (dev/?stress=chrono).
// - <name>.swd.jsonl: the SWD decoder's annotations, from the .chrono where there is one (then
//   with its recording_id), else from the .chronofx. Long captures keep only the rare kinds and
//   everything up to a tick: a full decode is the scale of a decoded track, not a sidecar.
// - <name>.annotations.json and .annotations.toml: a hand-written sidecar naming the first DPIDR
//   read, for the .chronofx and the .chrono route.
//
// The committed fixtures and generated files in fixtures/ are scripts/generate.ts's.

import { execFileSync } from "node:child_process";
import { existsSync, lstatSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { openChrono } from "./node-wasm.ts";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { values } = parseArgs({
  options: {
    exports: { type: "string" },
    energy: { type: "string" },
    converted: { type: "string", default: "" },
  },
});
const node = (...args: string[]) => execFileSync(process.execPath, args, { cwd: web, encoding: "utf8", maxBuffer: 1 << 30 });
const local = (name: string) => join(web, "fixtures-local", name);

process.stdout.write(
  node("scripts/extract-fixtures.ts", ...(values.exports ? ["--exports", values.exports] : []), ...(values.energy ? ["--energy", values.energy] : [])),
);

// The converted recordings the stress sections can switch to.
const CONVERTED = ["gcc-swd-reads", "stock-reads", "gcc-swd-matrix-capture100000", "session50"];
for (const name of CONVERTED) {
  const target = join(values.converted!, `${name}.1024.chrono`);
  const link = local(`${name}.chrono`);
  if (existsSync(link) || lstatSync(link, { throwIfNoEntry: false })) rmSync(link);
  if (!existsSync(target)) continue;
  symlinkSync(relative(dirname(link), target), link);
  console.log(`${link} -> ${target}`);
}

// The first DPIDR read of each, for the insets that compare them at one scale.
const STRESS = [
  { name: "gcc-swd-reads", until: 378700000, first: 378584398 },
  { name: "stock-swd-reads", until: 372000000, first: 371911979 },
];
for (const s of STRESS) {
  const chrono = existsSync(local(`${s.name}.chrono`));
  const input = chrono ? local(`${s.name}.chrono`) : local(`${s.name}.chronofx`);
  if (!existsSync(input)) continue;
  node("scripts/swd-annotations.ts", input, "--kinds", "reset,select,write,wait,fault,no-ack", "--until", String(s.until), "--out", local(`${s.name}.swd.jsonl`));
  const read = { id: "dpidr-read", ref: `swd/read/${s.first}`, label: "The first DPIDR read", measure: { channel: "SWCLK", what: "rate" } };
  const json = { format: "chrono-annotations/1", recording: `${s.name}.chronofx`, include: [`${s.name}.swd.jsonl`], annotation: [read] };
  writeFileSync(local(`${s.name}.annotations.json`), JSON.stringify(json, null, 2) + "\n");
  if (chrono) {
    const id = (await openChrono(input)).header.content_id;
    const toml = [
      `format = "chrono-annotations/1"`,
      `recording = "${s.name}.chrono"`,
      `recording_id = "${id}"`,
      `include = ["${s.name}.swd.jsonl"]`,
      ``,
      `[[annotation]]`,
      `id = "dpidr-read"`,
      `ref = "swd/read/${s.first}"`,
      `label = "The first DPIDR read"`,
      `measure = { channel = "SWCLK", what = "rate" }`,
    ].join("\n");
    writeFileSync(local(`${s.name}.annotations.toml`), toml + "\n");
  }
  console.log(`${s.name}: annotations from ${relative(web, input)}`);
}
