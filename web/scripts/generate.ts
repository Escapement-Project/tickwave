// Writes the files the tests and the dev page generate rather than commit. Run from web/:
//
//   node scripts/generate.ts
//
// - fixtures/identify-swd.swd.jsonl: the SWD decoder's annotations of the committed recording,
//   carrying its recording_id, which the committed TOML sidecar includes.
// - fixtures/identify-swd.bundle.tickwave: the recording with its sidecar bundled in,
//   fixtures/roles.bundle.tickwave with roles.annotations.toml's, fixtures/later.bundle.tickwave
//   with its sidecar as a later minor version would write it, a table this version doesn't know
//   added, and fixtures/later-lines.bundle.tickwave with its generator's file as one would write
//   it, a key this version doesn't know in every line, where the converter
//   (target/release/tickwave) has been built.

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const run = (cmd: string, args: string[]) => execFileSync(cmd, args, { cwd: web, stdio: ["ignore", "inherit", "inherit"] });

run(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave", "--out", "fixtures/identify-swd.swd.jsonl"]);
console.log("fixtures/identify-swd.swd.jsonl");

const cli = resolve(web, "../target/release/tickwave");
for (const f of ["identify-swd", "roles", "later", "later-lines"]) rmSync(resolve(web, `fixtures/${f}.bundle.tickwave`), { force: true });
if (!existsSync(cli)) console.log(`no ${cli}: the bundle test will skip`);
else {
  // A converter older than the sidecar's fields refuses it; that skips the bundle test, loudly,
  // rather than failing every other test.
  try {
    run(cli, ["bundle", "fixtures/identify-swd.tickwave", "fixtures/identify-swd.annotations.toml", "-o", "fixtures/identify-swd.bundle.tickwave"]);
    console.log("fixtures/identify-swd.bundle.tickwave");
    // A third role, stored looks and a stored view, as a downloaded copy carries them.
    run(cli, ["bundle", "fixtures/identify-swd.tickwave", "fixtures/roles.annotations.toml", "-o", "fixtures/roles.bundle.tickwave"]);
    console.log("fixtures/roles.bundle.tickwave");
    // The sidecar, and the file it includes, beside each other where nothing is committed.
    const later = mkdtempSync(resolve(tmpdir(), "tickwave-later-"));
    const toml = readFileSync(resolve(web, "fixtures/identify-swd.annotations.toml"), "utf8").replace('format = "tickwave-annotations/1"', 'format = "tickwave-annotations/1.1"');
    writeFileSync(resolve(later, "identify-swd.annotations.toml"), `${toml}\n[[decode]]\nid = "swd"\n`);
    copyFileSync(resolve(web, "fixtures/identify-swd.swd.jsonl"), resolve(later, "identify-swd.swd.jsonl"));
    run(cli, ["bundle", "fixtures/identify-swd.tickwave", resolve(later, "identify-swd.annotations.toml"), "-o", "fixtures/later.bundle.tickwave"]);
    console.log("fixtures/later.bundle.tickwave");
    const [head, ...lines] = readFileSync(resolve(web, "fixtures/identify-swd.swd.jsonl"), "utf8")
      .split("\n")
      .filter((l) => l.trim() !== "")
      .map((l) => JSON.parse(l) as Record<string, unknown>);
    const jsonl = [{ ...head, format: "tickwave-annotations/1.1" }, ...lines.map((l) => ({ ...l, confidence: "0.9" }))];
    copyFileSync(resolve(web, "fixtures/identify-swd.annotations.toml"), resolve(later, "identify-swd.annotations.toml"));
    writeFileSync(resolve(later, "identify-swd.swd.jsonl"), jsonl.map((l) => `${JSON.stringify(l)}\n`).join(""));
    run(cli, ["bundle", "fixtures/identify-swd.tickwave", resolve(later, "identify-swd.annotations.toml"), "-o", "fixtures/later-lines.bundle.tickwave"]);
    rmSync(later, { recursive: true });
    console.log("fixtures/later-lines.bundle.tickwave");
  } catch {
    console.log(`${cli} refused the sidecar (see above): the bundle test will skip`);
  }
}
