// Captures shared zipped, as GitHub takes them attached to an issue or a discussion: the Open page
// opens a zip as its files would, and a GitHub attachment's address shows the steps to download
// and choose it, since GitHub lets no other site read it. The zips are fixtures/zip/make.sh's.

import { test, expect, type Page } from "@playwright/test";
import { join } from "node:path";

const PAGE = "/dist/site/open.html";
const ZIPS = "fixtures/zip";
const IDENTIFY_ID = "blake3:63773d56fb3420735c864932f8d2441acd6f31a51b557e073bf27755a0534697";
const ATTACHMENT = "https://github.com/user-attachments/files/30440851/identify-swd.zip";

/** The facts list beside the view, as [term, value] pairs. */
async function facts(page: Page): Promise<Record<string, string>> {
  await expect(page.locator("#stage")).toBeVisible();
  return page.locator("#facts").evaluate((dl) => {
    const out: Record<string, string> = {};
    const items = [...dl.children];
    for (let i = 0; i + 1 < items.length; i += 2) out[items[i].textContent!] = items[i + 1].textContent!;
    return out;
  });
}

/** Fails the test on any request to GitHub: the page must not try to read an attachment. */
async function noGitHub(page: Page): Promise<string[]> {
  const asked: string[] = [];
  await page.route(/^https:\/\/([^/]*\.)?github(usercontent)?\.com\//, (route) => {
    asked.push(route.request().url());
    return route.abort();
  });
  return asked;
}

test("a zip opens as its files would, whichever tool wrote it", async ({ page }) => {
  await page.goto(PAGE);
  for (const zip of ["stored.zip", "deflated.zip", "streamed.zip", "macos.zip"]) {
    if (await page.locator("#opener").evaluate((d: HTMLDetailsElement) => !d.open)) await page.getByText("Open another recording").click();
    await page.locator("#files").setInputFiles(join(ZIPS, zip));
    const f = await facts(page);
    expect(f, zip).toMatchObject({ Recording: "identify-swd.tickwave", "Content id": IDENTIFY_ID });
    // The sidecar beside it in the zip, with its one annotation.
    await expect(page.locator("tickwave-view .steps .count"), zip).toHaveText("– of 1");
    await expect(page.locator("#problem"), zip).toBeHidden();
  }
});

test("a zip with several recordings lists them, and each opens with its own sidecar, the list staying put", async ({ page }) => {
  await page.goto(PAGE);
  await page.locator("#files").setInputFiles(join(ZIPS, "set.zip"));
  const pick = page.locator("#pick");
  await expect(pick.locator("legend")).toHaveText("There are 2 recordings here. Choose one to open:");
  await expect(pick.locator("label")).toHaveText(["identify-jtag.tickwave", "identify-swd.tickwave, with its annotations"]);
  await expect(page.locator("#stage")).toBeHidden();
  // Where the list is on the page: an opened view scrolls itself into sight.
  const onPage = () => pick.evaluate((p: HTMLElement) => ({ top: p.getBoundingClientRect().top + scrollY, height: p.offsetHeight }));
  const at = await onPage();
  await pick.getByLabel("identify-swd.tickwave").check();
  expect((await facts(page)).Recording).toBe("identify-swd.tickwave");
  await expect(page.locator("tickwave-view .steps .count")).toHaveText("– of 1");
  await pick.getByLabel("identify-jtag.tickwave").check();
  expect((await facts(page)).Recording).toBe("identify-jtag.tickwave");
  await expect(page.locator("tickwave-view .steps")).toBeHidden();
  const now = await onPage();
  // Firefox's top can be off by 2^-15 px from one measure to the next.
  expect(now.top).toBeCloseTo(at.top, 2);
  expect(now.height).toBe(at.height);
  await expect(page.locator("#problem")).toBeHidden();
});

test("a capture in a zip converts", async ({ page }) => {
  await page.goto(PAGE);
  await page.locator("#files").setInputFiles(join(ZIPS, "capture.zip"));
  await expect(page.locator("#save")).toBeVisible({ timeout: 30_000 });
  await expect(page.locator("#source")).toHaveText("demo-logic.vcd");
  await expect(page.locator("#channels tr")).toHaveCount(8);
});

test("a zip it can't open says why", async ({ page }) => {
  await page.goto(PAGE);
  const problem = page.locator("#problem");
  const cases: [string, string][] = [
    ["encrypted.zip", "encrypted.zip couldn't be unzipped: identify-swd.tickwave is encrypted."],
    ["bzip2.zip", "bzip2.zip couldn't be unzipped: demo-logic.vcd is compressed with Bzip2, and tickwave reads only stored and deflated files."],
    ["duplicate.zip", "duplicate.zip couldn't be unzipped: a/identify-swd.tickwave and b/identify-swd.tickwave share a name, so either could be the one meant."],
    ["nothing.zip", "nothing.zip holds nothing to open or convert: no .tickwave recording, and no capture (a Saleae export's .bin files, or a sigrok .sr or .vcd file)."],
  ];
  for (const [zip, says] of cases) {
    await page.locator("#files").setInputFiles(join(ZIPS, zip));
    await expect(problem).toHaveText(says);
    await expect(page.locator("#stage")).toBeHidden();
  }
  await page.locator("#files").setInputFiles({ name: "notes.zip", mimeType: "application/zip", buffer: Buffer.from("not a zip") });
  await expect(problem).toHaveText(/^notes\.zip couldn't be unzipped: it isn't a zip archive tickwave can read: /);
});

test("a GitHub attachment's link shows the steps to download and choose it, and fetches nothing from GitHub", async ({ page }) => {
  const asked = await noGitHub(page);
  await page.goto(`${PAGE}?src=${encodeURIComponent(ATTACHMENT)}`);
  const attached = page.locator("#attached");
  await expect(attached.getByRole("heading")).toHaveText("Open identify-swd.zip");
  await expect(attached.getByRole("link", { name: "Download it from GitHub" })).toHaveAttribute("href", ATTACHMENT);
  await expect(attached.getByRole("link", { name: "Download it from GitHub" })).toBeFocused();
  await expect(page.locator("#attached-share a")).toHaveAttribute("href", page.url());
  expect(new URL(page.url()).searchParams.get("src")).toBe(ATTACHMENT);
  await expect(page.locator("#problem")).toBeHidden();
  await expect(page.locator("#opener")).not.toHaveAttribute("open");

  // The file the reader downloaded opens, and the steps give way to it.
  await page.locator("#attached-file").setInputFiles(join(ZIPS, "deflated.zip"));
  expect((await facts(page)).Recording).toBe("identify-swd.tickwave");
  await expect(attached).toBeHidden();

  // A link written by hand, the address unencoded, as viewer.md shows one.
  await page.goto(`${PAGE}?src=${ATTACHMENT}`);
  await expect(attached.getByRole("heading")).toHaveText("Open identify-swd.zip");
  expect(asked).toEqual([]);
});

test("an attachment's address in the form gives the link to post; one that only looks like GitHub's is fetched as any other", async ({ page }) => {
  const asked = await noGitHub(page);
  await page.goto(PAGE);
  await page.locator("#src").fill(ATTACHMENT);
  await page.getByRole("button", { name: "Open" }).click();
  await expect(page.locator("#attached")).toBeVisible();
  const link = `${new URL(PAGE, page.url()).href}?src=${encodeURIComponent(ATTACHMENT)}`;
  await expect(page.locator("#attached-share a")).toHaveAttribute("href", link);
  expect(asked).toEqual([]);

  const lookalike = "https://github.com.evil.example/user-attachments/files/1/identify-swd.zip";
  await page.route("https://github.com.evil.example/**", (route) => route.abort());
  await page.goto(`${PAGE}?src=${encodeURIComponent(lookalike)}`);
  await expect(page.locator("#problem")).toBeVisible();
  await expect(page.locator("#attached")).toBeHidden();
});

test("on a phone, the steps fit the screen and the downloaded zip opens from the file picker", async ({ browser, baseURL }) => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true });
  const page = await context.newPage();
  await page.goto(new URL(`${PAGE}?src=${encodeURIComponent(ATTACHMENT)}`, baseURL).href);
  const attached = page.locator("#attached");
  await expect(attached).toBeVisible();
  for (const control of [attached.getByRole("link", { name: "Download it from GitHub" }), attached.locator("label.button")]) {
    const box = (await control.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390);
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  await page.locator("#attached-file").setInputFiles(join(ZIPS, "macos.zip"));
  expect((await facts(page)).Recording).toBe("identify-swd.tickwave");
  await context.close();
});
