// Rebuilds every fixture and its annotations. Run from web/: node scripts/fixtures.ts
//
// The committed fixture's hand-written annotations (fixtures/identify-swd.annotations.json) are
// kept as they are; the ignored ones in fixtures-local/ are written here.

import { execFileSync } from "node:child_process";
import { existsSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const node = (...args: string[]) => execFileSync(process.execPath, args, { cwd: web, encoding: "utf8", maxBuffer: 1 << 30 });

process.stdout.write(node("scripts/extract-fixtures.ts", ...process.argv.slice(2)));

const swd = (fixture: string, ...args: string[]) => {
  if (!existsSync(join(web, fixture))) return false;
  writeFileSync(join(web, fixture.replace(/\.chronofx$/, ".swd.jsonl")), node("scripts/swd-annotations.ts", fixture, ...args));
  return true;
};

swd("fixtures/identify-swd.chronofx");

// The first DPIDR read of each, for the insets that compare them at one scale.
const STRESS = [
  { name: "gcc-swd-reads", until: 378700000, first: 378584398 },
  { name: "stock-swd-reads", until: 372000000, first: 371911979 },
];
for (const s of STRESS) {
  if (!swd(`fixtures-local/${s.name}.chronofx`, "--kinds", "reset,select,write,wait,fault,no-ack", "--until", String(s.until))) continue;
  const sidecar = {
    format: "chrono-annotations/1",
    recording: `${s.name}.chronofx`,
    include: [`${s.name}.swd.jsonl`],
    annotation: [
      { id: "dpidr-read", ref: `swd/read/${s.first}`, label: "The first DPIDR read", measure: { channel: "SWCLK", what: "rate" } },
    ],
  };
  writeFileSync(join(web, `fixtures-local/${s.name}.annotations.json`), JSON.stringify(sidecar, null, 2) + "\n");
}
console.log("annotations written");
