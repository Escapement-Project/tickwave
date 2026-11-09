// Writes the files the tests and the dev page generate rather than commit. Run from web/:
//
//   node scripts/generate.ts
//
// - fixtures/identify-swd.swd.jsonl: the SWD decoder's annotations of the committed recording,
//   carrying its recording_id, which the committed TOML sidecar includes.
// - fixtures/identify-swd.bundle.tickwave: the recording with its sidecar bundled in, where the
//   converter (target/release/tickwave) has been built.

import { execFileSync } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const run = (cmd: string, args: string[]) => execFileSync(cmd, args, { cwd: web, stdio: ["ignore", "inherit", "inherit"] });

run(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave", "--out", "fixtures/identify-swd.swd.jsonl"]);
console.log("fixtures/identify-swd.swd.jsonl");

const cli = resolve(web, "../target/release/tickwave");
rmSync(resolve(web, "fixtures/identify-swd.bundle.tickwave"), { force: true });
if (!existsSync(cli)) console.log(`no ${cli}: the bundle test will skip`);
else {
  // A converter older than the sidecar's fields refuses it; that skips the bundle test, loudly,
  // rather than failing every other test.
  try {
    run(cli, ["bundle", "fixtures/identify-swd.tickwave", "fixtures/identify-swd.annotations.toml", "-o", "fixtures/identify-swd.bundle.tickwave"]);
    console.log("fixtures/identify-swd.bundle.tickwave");
  } catch {
    console.log(`${cli} refused the sidecar (see above): the bundle test will skip`);
  }
}
