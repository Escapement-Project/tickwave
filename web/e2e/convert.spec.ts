// The converter on the site's Open page, built into dist/site/. A capture converts at once with every
// channel, and Save converts it again with the table's settings when they change anything. Each conversion's saved file must be the CLI's, byte
// for byte, from the same capture and settings; the CLI runs through cargo.

import { test, expect, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { v0Copy } from "../test/saleae-v0.ts";

const PAGE = "/dist/site/open.html";
const REPO = new URL("../..", import.meta.url).pathname;
const DATA = join(REPO, "testdata");

function cli(args: string[]): Buffer {
  const out = join(mkdtempSync(join(tmpdir(), "escapement-tickwave-cli-")), "out.tickwave");
  execFileSync("cargo", ["run", "-q", "--release", "-p", "escapement-tickwave-cli", "--", "convert", ...args, "-o", out], { cwd: REPO, stdio: ["ignore", "ignore", "pipe"] });
  return readFileSync(out);
}

/** Waits for a capture's first conversion, which shows it with Save focused. */
async function converted(page: Page): Promise<void> {
  const save = page.locator("#save");
  await expect(save).toBeVisible({ timeout: 30_000 });
  // The next thing to do is save it.
  await expect(save).toBeFocused();
}

/** Save, and the bytes of the recording it gives: the capture as converted when nothing changed,
 * else a conversion with the table's settings first. */
async function save(page: Page): Promise<Buffer> {
  const [download] = await Promise.all([page.waitForEvent("download", { timeout: 30_000 }), page.locator("#save").click()]);
  return readFileSync(await download.path());
}

/** The first conversion, saved as it is. */
async function saveConverted(page: Page): Promise<Buffer> {
  await converted(page);
  return save(page);
}

test("a Saleae export's folder: its channels in number order, named and converted as the CLI converts them", async ({ page }) => {
  await page.goto(PAGE);
  await page.locator("#folder").setInputFiles(join(DATA, "saleae/swd50-cut"));
  // It converts at once, every channel under the capture's own name.
  const first = await saveConverted(page);
  const all = ["--channel", "analog_0=analog_0", "--channel", "analog_1=analog_1", "--channel", "digital_4_0=digital_4_0"];
  expect(first.equals(cli(["saleae", join(DATA, "saleae/swd50-cut"), ...all])), "the first recording is the CLI's").toBe(true);
  // Saved, it opens as a recording; Shape again goes back to the capture.
  await expect(page.locator("#settings")).toBeHidden();
  await page.getByRole("button", { name: "Shape again" }).click();
  await expect(page.locator("#source")).toHaveText("swd50-cut");
  await expect(page.locator("#channels tr")).toHaveText([/^analog_0 analog, 50,000 samples/, /^analog_1 analog, 50,000 samples/, /^digital_4_0 digital, 0 edges/]);
  await expect(page.getByLabel("Unit for analog_0")).toHaveValue("V");
  await page.getByLabel("Name for analog_0").fill("VTREF");
  await page.getByLabel("Name for analog_1").fill("I");
  await page.getByLabel("Unit for analog_1").fill("mA");
  await page.getByLabel("Name for digital_4_0").fill("SWCLK");
  const saved = await save(page);
  await expect(page.locator("#save")).toHaveText("Save swd50-cut.tickwave");
  await expect(page.locator("#found")).toHaveText("Exact codes: VTREF (12-bit), I (10-bit).");
  const native = cli(["saleae", join(DATA, "saleae/swd50-cut"), "--channel", "analog_0=VTREF", "--channel", "analog_1=I", "--channel", "digital_4_0=SWCLK", "--unit", "I=mA"]);
  expect(saved.equals(native), "the saved recording is the CLI's").toBe(true);
  // The recording saved opens, with its facts; Shape again brings the settings back as they were.
  await expect(page.locator("#facts")).toContainText("VTREF (analog, V), I (analog, mA), SWCLK");
  await expect(page.locator("#stage tickwave-view")).toBeVisible();
  await expect(page.locator("#stage tickwave-view")).not.toHaveAttribute("shaping");
  await page.getByRole("button", { name: "Shape again" }).click();
  await expect(page.getByLabel("Name for analog_0")).toHaveValue("VTREF");
  await expect(page.locator("#stage tickwave-view")).toHaveAttribute("shaping", "");
});

test("a version 0 Saleae export: the page asks for the rate it doesn't record, infers it when left empty, and takes one given", async ({ page }) => {
  const v0 = v0Copy(join(DATA, "saleae/identify-swd"), join(mkdtempSync(join(tmpdir(), "tickwave-v0-")), "identify-swd-v0"));
  await page.goto(PAGE);
  await page.locator("#folder").setInputFiles(v0);
  // The first conversion infers the rate; the table then asks for one.
  const inferred = await saveConverted(page);
  const channels = ["--channel", "digital_3_0=digital_3_0", "--channel", "digital_3_1=digital_3_1", "--channel", "digital_3_2=digital_3_2"];
  await page.getByRole("button", { name: "Shape again" }).click();
  await expect(page.locator("#source")).toHaveText("identify-swd-v0");
  const rate = page.getByLabel("Sample rate");
  await expect(rate).toBeVisible();
  await expect(rate).toHaveValue("");
  await expect(rate).toHaveAccessibleDescription(/doesn't record its digital channels' sample rate/);
  expect(inferred.equals(cli(["saleae", v0, ...channels])), "the inferred rate's recording is the CLI's").toBe(true);
  await expect(page.locator("#facts")).toContainText("Sample rateinferred from the edges");
  await rate.fill("500M");
  await page.locator("#save").click();
  await expect(page.locator("#problem")).toContainText("off the sample grid");
  await rate.fill("1G");
  const given = await save(page);
  expect(given.equals(cli(["saleae", v0, ...channels, "--rate", "1G"])), "the given rate's recording is the CLI's").toBe(true);
  await expect(page.locator("#facts")).toContainText("Sample rategiven when it was converted");
  // An export that records its rate asks for none.
  await page.getByText("Open another recording").click();
  await page.locator("#folder").setInputFiles(join(DATA, "saleae/swd50-cut"));
  await converted(page);
  await expect(page.locator("#source")).toHaveText("swd50-cut");
  await expect(rate).toBeHidden();
});

test("one chooser: a capture converts and shows the converter's settings, and a recording chosen after it opens in their place", async ({ page }) => {
  await page.goto(PAGE);
  await page.locator("#files").setInputFiles(join(DATA, "import/demo-logic.vcd"));
  await converted(page);
  await expect(page.locator("#source")).toHaveText("demo-logic.vcd");
  await expect(page.locator("#settings")).toBeVisible();
  await page.getByText("Open another recording").click();
  await page.locator("#files").setInputFiles("fixtures/identify-swd.tickwave");
  await expect(page.locator("#stage tickwave-view")).toBeVisible();
  await expect(page.locator("#facts")).toContainText("SWCLK, SWDIO");
  await expect(page.locator("#settings")).toBeHidden();
  await expect(page.locator("#saving")).toBeHidden();
  // And a capture after a recording puts the recording away.
  await page.getByText("Open another recording").click();
  await page.locator("#files").setInputFiles(join(DATA, "import/demo-logic.vcd"));
  await converted(page);
  await expect(page.locator("#settings")).toBeVisible();
  await expect(page.locator("#facts")).toContainText("demo-logic.vcd");
});

test("a sigrok file with a channel left out, one renamed, a title, metadata and other encoding", async ({ page }) => {
  await page.goto(PAGE);
  await page.locator("#files").setInputFiles(join(DATA, "import/demo-mixed.sr"));
  await converted(page);
  await expect(page.locator("#source")).toHaveText("demo-mixed.sr");
  await page.getByLabel("Take D0").uncheck();
  await page.getByLabel("Name for A1").fill("I");
  await page.locator("#title").fill("demo, mixed");
  await page.locator("#settings summary").click();
  await page.locator("#level").fill("5");
  await page.locator("#block").fill("16");
  await page.getByLabel(/exact integer codes/).uncheck();
  await page.locator("#meta").fill("probe = verge\n\nsource=sigrok demo ");
  const saved = await save(page);
  const channels = ["D1", "D2", "D3", "D4", "D5", "D6", "D7", "D8", "D9", "D10", "D11", "A0", "A1=I"].flatMap((c) => ["--channel", c.includes("=") ? c : `${c}=${c}`]);
  const native = cli(["sigrok", join(DATA, "import/demo-mixed.sr"), ...channels, "--title", "demo, mixed", "--level", "5", "--block-kib", "16", "--no-codes", "--meta", "probe=verge", "--meta", "source=sigrok demo"]);
  expect(saved.equals(native), "the saved recording is the CLI's").toBe(true);
});

test("a dropped VCD file converts with every one-bit signal", async ({ page }) => {
  await page.goto(PAGE);
  const text = readFileSync(join(DATA, "import/demo-logic.vcd"), "utf8");
  await page.locator("#drop").evaluate((drop, text) => {
    const data = new DataTransfer();
    data.items.add(new File([text], "demo-logic.vcd"));
    drop.dispatchEvent(new DragEvent("drop", { dataTransfer: data, bubbles: true, cancelable: true }));
  }, text);
  const saved = await saveConverted(page);
  await expect(page.locator("#source")).toHaveText("demo-logic.vcd");
  expect(saved.equals(cli(["vcd", join(DATA, "import/demo-logic.vcd")])), "the saved recording is the CLI's").toBe(true);
});

test("says what is wrong: no capture, two captures, a file that is not one, and the form's own mistakes", async ({ page }) => {
  const dir = mkdtempSync(join(tmpdir(), "tickwave-convert-"));
  const notes = join(dir, "notes.txt");
  writeFileSync(notes, "nothing to convert");
  const bad = join(dir, "analog_9.bin");
  writeFileSync(bad, "not an export either");
  await page.goto(PAGE);
  const problem = page.locator("#problem");

  await page.locator("#files").setInputFiles(notes);
  await expect(problem).toHaveText(/^There's nothing here to open or convert/);
  await page.locator("#files").setInputFiles([join(DATA, "import/marker.sr"), join(DATA, "import/marker.vcd")]);
  await expect(problem).toHaveText(/^Choose one capture at a time/);
  await page.locator("#files").setInputFiles([join(DATA, "saleae/identify-swd/digital_3_0.bin"), bad]);
  await converted(page);
  await expect(problem).toBeHidden();
  await expect(page.locator("#channels tr")).toHaveCount(1);
  await expect(page.locator("#skipped")).toHaveText("Left out: analog_9.bin: analog_9.bin: not a Saleae binary export.");

  await page.getByLabel("Name for digital_3_0").fill(" ");
  await page.locator("#save").click();
  await expect(problem).toHaveText("Give digital_3_0 a name.");
  await page.getByLabel("Take digital_3_0").uncheck();
  await page.locator("#save").click();
  await expect(problem).toHaveText("Take at least one channel.");
  await page.getByLabel("Take digital_3_0").check();
  await page.getByLabel("Name for digital_3_0").fill("SWCLK");
  await page.locator("#settings summary").click();
  await page.locator("#meta").fill("no equals sign");
  await page.locator("#save").click();
  await expect(problem).toHaveText("Each metadata line is key = value; this one isn't: no equals sign");
});
