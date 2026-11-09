// Writes the files the tests and the dev page generate rather than commit. Run from web/:
//
//   node scripts/generate.ts
//
// - fixtures/identify-swd.swd.jsonl: the SWD decoder's annotations of the committed recording,
//   carrying its recording_id, which the committed TOML sidecar includes.
// - fixtures/identify-swd.bundle.chrono: the recording with its sidecar bundled in, where the
//   converter (target/release/chrono) has been built.

import { execFileSync } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const run = (cmd: string, args: string[]) => execFileSync(cmd, args, { cwd: web, stdio: ["ignore", "inherit", "inherit"] });

run(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.chrono", "--out", "fixtures/identify-swd.swd.jsonl"]);
console.log("fixtures/identify-swd.swd.jsonl");

const cli = resolve(web, "../target/release/chrono");
rmSync(resolve(web, "fixtures/identify-swd.bundle.chrono"), { force: true });
if (existsSync(cli)) {
  run(cli, ["bundle", "fixtures/identify-swd.chrono", "fixtures/identify-swd.annotations.toml", "-o", "fixtures/identify-swd.bundle.chrono"]);
  console.log("fixtures/identify-swd.bundle.chrono");
} else console.log(`no ${cli}: the bundle test will skip`);
