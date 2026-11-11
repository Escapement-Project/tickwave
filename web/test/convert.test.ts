// The browser's converter, run in Node through the same WASM module and requests: for every test
// capture it must write the CLI's bytes exactly, with the same settings. The CLI runs through cargo
// (crates/convert-wasm/build.sh builds the module first).

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { convertFiles, describeFiles } from "../scripts/node-convert.ts";
import type { Settings, SourceKind } from "../src/convert/protocol.ts";

const REPO = new URL("../..", import.meta.url).pathname;
const DATA = join(REPO, "testdata");
const OUT = mkdtempSync(join(tmpdir(), "tickwave-convert-"));

function cli(args: string[]): void {
  execFileSync("cargo", ["run", "-q", "--release", "-p", "escapement-tickwave-cli", "--", ...args], { cwd: REPO, stdio: ["ignore", "ignore", "pipe"] });
}

interface Case {
  name: string;
  kind: SourceKind;
  /** A Saleae export's directory, or the one file. */
  source: string;
  settings: Partial<Settings>;
  args: string[];
}

const CASES: Case[] = [
  {
    name: "identify-swd, titled, with metadata",
    kind: "saleae",
    source: "saleae/identify-swd",
    settings: { channels: [["digital_3_0", "SWCLK"], ["digital_3_1", "SWDIO"], ["digital_3_2", "NC"]], title: "identify", meta: [["probe", "verge"]] },
    args: ["--channel", "digital_3_0=SWCLK", "--channel", "digital_3_1=SWDIO", "--channel", "digital_3_2=NC", "--title", "identify", "--meta", "probe=verge"],
  },
  {
    name: "swd50-cut, analog codes and a unit",
    kind: "saleae",
    source: "saleae/swd50-cut",
    settings: { channels: [["digital_4_0", "SWCLK"], ["analog_0", "VTREF"], ["analog_1", "I"]], units: [["I", "mA"]] },
    args: ["--channel", "digital_4_0=SWCLK", "--channel", "analog_0=VTREF", "--channel", "analog_1=I", "--unit", "I=mA"],
  },
  {
    name: "swd50-cut as f32, level 3, 64 KiB blocks",
    kind: "saleae",
    source: "saleae/swd50-cut",
    settings: { channels: [["analog_0", "VTREF"]], codes: false, level: 3, blockKib: 64 },
    args: ["--channel", "analog_0=VTREF", "--no-codes", "--level", "3", "--block-kib", "64"],
  },
  ...["demo-analog", "demo-logic", "demo-mixed", "demo-disabled", "marker", "markers-1000"].map(
    (f): Case => ({ name: `${f}.sr`, kind: "sigrok", source: `import/${f}.sr`, settings: {}, args: [] }),
  ),
  { name: "demo-mixed.sr, two channels renamed", kind: "sigrok", source: "import/demo-mixed.sr", settings: { channels: [["A1", "I"], ["D3", "CLK"]] }, args: ["--channel", "A1=I", "--channel", "D3=CLK"] },
  ...["demo-logic", "demo-mixed", "marker", "markers-1000"].map((f): Case => ({ name: `${f}.vcd`, kind: "vcd", source: `import/${f}.vcd`, settings: {}, args: [] })),
];

for (const c of CASES) {
  test(`the browser's converter writes the CLI's bytes: ${c.name}`, () => {
    const source = join(DATA, c.source);
    const paths = c.kind === "saleae" ? readdirSync(source).map((f) => join(source, f)) : [source];
    const native = join(OUT, `${c.name}.cli.tickwave`);
    cli(["convert", c.kind, source, "-o", native, ...c.args]);
    const { file } = convertFiles(c.kind, paths, c.settings);
    writeFileSync(join(OUT, `${c.name}.wasm.tickwave`), file);
    assert.ok(Buffer.from(file).equals(readFileSync(native)), `${c.name}: the bytes differ (both in ${OUT})`);
  });
}

test("a Saleae export's passes: its edges checked once, then codes, encoding and the check against it", () => {
  const dir = join(DATA, "saleae/swd50-cut");
  const passes: string[] = [];
  convertFiles(
    "saleae",
    readdirSync(dir).map((f) => join(dir, f)),
    { channels: [["digital_4_0", "SWCLK"], ["analog_0", "VTREF"]] },
    { start: (pass, channel, total) => passes.push(`${pass}${channel ? ` ${channel}` : ""} ${total}`) },
  );
  assert.deepEqual(passes, ["check 0", "codes VTREF 50000", "encode 50000", "verify 50000"]);
  const identify = join(DATA, "saleae/identify-swd");
  const steps: number[] = [];
  convertFiles("saleae", readdirSync(identify).map((f) => join(identify, f)), { channels: [["digital_3_0", "SWCLK"], ["digital_3_1", "SWDIO"]] }, {
    start: (pass, _, total) => passes.push(`${pass} ${total}`),
    step: (n) => steps.push(n),
  });
  assert.deepEqual(passes.slice(4), ["check 17160", "encode 17160", "verify 17160"]);
  assert.deepEqual(steps, [], "fewer than 65,536 items a pass report no step");
});

test("a Saleae export's channels, in number order, and a file that is not one, skipped", () => {
  const dir = mkdtempSync(join(tmpdir(), "tickwave-describe-"));
  for (const f of readdirSync(join(DATA, "saleae/swd50-cut"))) copyFileSync(join(DATA, "saleae/swd50-cut", f), join(dir, f));
  copyFileSync(join(DATA, "saleae/swd50-cut/analog_0.bin"), join(dir, "analog_10.bin"));
  writeFileSync(join(dir, "analog_2.bin"), "not an export");
  writeFileSync(join(dir, "notes.txt"), "left out: not a .bin");
  const described = describeFiles("saleae", readdirSync(dir).map((f) => join(dir, f)));
  assert.deepEqual(
    described.channels.map((c) => [c.name, c.analog, c.count, c.unit]),
    [
      ["analog_0", true, 50000, "V"],
      ["analog_1", true, 50000, "V"],
      ["analog_10", true, 50000, "V"],
      ["digital_4_0", false, 0, null],
    ],
  );
  assert.deepEqual(described.skipped, [{ name: "analog_2.bin", reason: "analog_2.bin: not a Saleae binary export" }]);
});

test("a source's problem comes back as its message, and a channel not there names those that are", () => {
  const dir = join(DATA, "saleae/identify-swd");
  const paths = readdirSync(dir).map((f) => join(dir, f));
  assert.throws(() => convertFiles("saleae", paths, { channels: [["digital_9_0", "X"]] }), /no digital_9_0\.bin among the files/);
  assert.throws(() => convertFiles("vcd", [join(DATA, "import/marker.vcd")], { channels: [["nope", "X"]] }), /no channel "nope"; the source has/);
});
