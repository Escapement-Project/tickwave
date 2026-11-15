// The Open page's merge: a recording opened from this computer, another added beside it, lined up
// by its head's field, a channel left out, and saved. Save writes what `tickwave merge` writes with
// the same settings, and `tickwave verify` checks it against the two. identify-swd ticks at 1 GHz
// and the energy fixture at 96 MHz, so they merge at 12 GHz: 1 ms is 12,000,000 ticks. identify-swd
// opens with its sidecar, which the CLI reads beside it: its decoder's annotations come along.

import { test, expect, type Locator, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const REPO = new URL("../..", import.meta.url).pathname;
const FIXTURES = join(REPO, "web/fixtures");
const DATA = join(REPO, "testdata");

function tickwave(args: string[]): string {
  return execFileSync("cargo", ["run", "-q", "--release", "-p", "escapement-tickwave-cli", "--", ...args], { cwd: REPO, stdio: ["ignore", "pipe", "pipe"], encoding: "utf8" });
}

/** Folders a test made, removed after it. */
const made: string[] = [];
test.afterEach(() => {
  for (const dir of made.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function scratch(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  made.push(dir);
  return dir;
}

type Grouped = { groups: { name: string; channels: string[] }[]; aligns: Record<string, number> };

async function drawn(view: Locator): Promise<void> {
  await view.evaluate((v) => new Promise<void>((r) => (v.shadowRoot!.querySelector("canvas")!.width > 0 ? r() : v.addEventListener("tickwave-ready", () => r(), { once: true }))));
}

/** identify-swd opened with its sidecar, and the energy fixture added to it. */
async function merging(page: Page): Promise<Locator> {
  await page.goto("/dist/site/open.html");
  await page.locator("#files").setInputFiles(["identify-swd.tickwave", "identify-swd.annotations.toml", "identify-swd.swd.jsonl"].map((f) => join(FIXTURES, f)));
  await expect(page.locator("#adding")).toBeVisible();
  await page.locator("#add-files").setInputFiles(join(FIXTURES, "energy.tickwave"));
  await expect(page.locator("#channels tr.input")).toHaveCount(2);
  const view = page.locator("tickwave-view");
  await expect(view).toHaveAttribute("shaping", "");
  await drawn(view);
  return view;
}

test("a recording added merges with the one shown, each under its head, lined up by its field, and Save writes what tickwave merge does", async ({ page }) => {
  const view = await merging(page);
  expect(await view.evaluate((v) => (v as unknown as Grouped).groups.map((g) => [g.name, g.channels]))).toEqual([
    ["identify-swd.tickwave", ["SWCLK", "SWDIO"]],
    ["energy.tickwave", ["V", "I", "range", "under_range", "DIO0", "P"]],
  ]);
  // P, derived from I and V, has no row of its own: it comes with them.
  await expect(page.locator("#channels tr[data-source]")).toHaveCount(7);
  const align = page.getByLabel("Align energy.tickwave by");
  await align.fill("1 ms");
  await align.press("Tab");
  expect((await view.evaluate((v) => (v as unknown as Grouped).aligns))["energy.tickwave"]).toBe(12_000_000);
  await page.getByLabel("Take under_range").uncheck();
  await expect(view).toHaveAttribute("channels", "SWCLK SWDIO V I P range DIO0");
  // A channel's move goes to the merge in its own instrument's ticks: 1 µs is 12,000 merged ticks,
  // and 96 of the energy fixture's 96 MHz ones.
  const move = page.getByLabel("Move DIO0 by");
  await move.fill("-1 µs");
  await move.press("Tab");
  expect((await view.evaluate((v) => (v as unknown as { shape: Record<string, { offset: number }> }).shape)).DIO0.offset).toBe(-12_000);
  const [download] = await Promise.all([page.waitForEvent("download", { timeout: 60_000 }), page.locator("#save").click()]);
  expect(download.suggestedFilename()).toBe("identify-swd+energy.tickwave");
  const saved = readFileSync((await download.path())!);
  // The CLI with the same settings, to a file of the same name, which names its bundled files.
  const dir = scratch("escapement-tickwave-merge-");
  const out = join(dir, "identify-swd+energy.tickwave");
  const flags = [...["SWCLK", "SWDIO"].flatMap((c) => ["--channel", `1:${c}=${c}`]), ...["V", "I", "range", "DIO0"].flatMap((c) => ["--channel", `2:${c}=${c}`]), "--align", "2=1ms", "--offset", "DIO0=-1us"];
  tickwave(["merge", join(FIXTURES, "identify-swd.tickwave"), join(FIXTURES, "energy.tickwave"), ...flags, "-o", out]);
  expect(saved.equals(readFileSync(out)), `the saved recording is tickwave merge's with ${flags.join(" ")}`).toBe(true);
  const copy = join(dir, "saved.tickwave");
  writeFileSync(copy, saved);
  expect(() => tickwave(["verify", copy, "--against", join(FIXTURES, "identify-swd.tickwave"), "--against", join(FIXTURES, "energy.tickwave")])).not.toThrow();
  // identify-swd's decoder's annotations came with it, from its sidecar, and the energy fixture's
  // bundled ones with it, under the merged recording's id; the hand-written ones did not.
  const bundled = JSON.parse(execFileSync(process.execPath, ["scripts/bundled.ts", copy], { encoding: "utf8" })) as string[];
  expect(bundled).toEqual(["identify-swd+energy.annotations.toml", "identify-swd+energy.swd.jsonl", "identify-swd+energy.verge.jsonl"]);
  await expect(page.locator("#found")).toContainText("hand-written");
  // The merged recording opens as saved, and Shape again goes back to the merge as it was.
  await expect(view).not.toHaveAttribute("shaping");
  await expect(page.locator("#took")).toContainText("merged in");
  await page.getByRole("button", { name: "Shape again" }).click();
  await expect(page.locator("#channels tr.input")).toHaveCount(2);
  await expect(page.getByLabel("Take under_range")).not.toBeChecked();
  await drawn(page.locator("tickwave-view"));
  expect((await page.locator("tickwave-view").evaluate((v) => (v as unknown as Grouped).aligns))["energy.tickwave"]).toBe(12_000_000);
});

test("Remove takes a recording out, and with one left the page shows it alone", async ({ page }) => {
  await merging(page);
  await page.getByRole("button", { name: "Remove energy.tickwave" }).click();
  await expect(page.locator("#channels tr.input")).toHaveCount(0);
  const view = page.locator("tickwave-view");
  await drawn(view);
  await expect(view).not.toHaveAttribute("shaping");
  await expect(page.locator("#facts")).toContainText("identify-swd.tickwave");
  await expect(page.locator("#adding")).toBeVisible();
});

test("a capture added converts whole first, and a capture being shaped takes a recording", async ({ page }) => {
  await page.goto("/dist/site/open.html");
  await page.locator("#files").setInputFiles(join(DATA, "import/demo-logic.vcd"));
  await expect(page.locator("#save")).toBeVisible({ timeout: 30_000 });
  // The capture's table, as converted, carries over as its rows in the merge.
  const first = page.locator("#channels tr[data-source]").first();
  const name = await first.getAttribute("data-source");
  await first.locator("input.name").fill("RENAMED");
  await page.locator("#add-files").setInputFiles(join(FIXTURES, "middle-gap.tickwave"));
  await expect(page.locator("#channels tr.input")).toHaveCount(2);
  await expect(page.locator(`#channels tr[data-source="${name}"] input.name`)).toHaveValue("RENAMED");
  await expect(page.locator("#channels tr.input th").first()).toContainText("demo-logic.vcd");
  // Adding a capture to the merge converts it, and it joins as a third recording.
  await page.locator("#add-files").setInputFiles(join(DATA, "import/marker.vcd"));
  await expect(page.locator("#channels tr.input")).toHaveCount(3, { timeout: 30_000 });
  await expect(page.locator("#channels tr.input th").nth(2)).toContainText("marker.vcd");
});
